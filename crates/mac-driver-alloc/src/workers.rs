//! Captured native module workers. Preparation allocates captured state under
//! the module's real Kernel account; failed preparation returns the body intact.
//! The resident owns closure destruction after CREATE and retains code/stack
//! pins until cleanup actually finishes. Drop requests cancellation/disposal and
//! never waits for uncooperative privileged code.
use core::{alloc::{GlobalAlloc, Layout},
           ffi::c_void,
           marker::PhantomData,
           mem,
           ptr::NonNull};

use mac_abi::{BAD_ABI, BAD_ARGUMENT, NO_MEMORY, OK};
use mac_driver::{Client, Error, Result, abi,
                 runtime::workers::{self as transport, WorkerHandle, WorkerState}};

use crate::ForwardingAllocator;

/// Uninitialized captured state uses the resident forwarder explicitly, even
/// when an author selected a different global allocator for unrelated storage.
struct CaptureReservation<F>
{
    pointer: NonNull<F>
}
impl<F> CaptureReservation<F>
{
    fn try_new() -> Result<Self>
    {
        let layout = Layout::new::<F>();
        let pointer = if layout.size() == 0
        {
            NonNull::dangling()
        }
        else
        {
            // SAFETY: exact nonzero F layout; no value is initialized yet.
            NonNull::new(unsafe { ForwardingAllocator.alloc(layout) }.cast::<F>())
                .ok_or(Error(NO_MEMORY))?
        };
        Ok(Self { pointer })
    }

    fn write(self, body: F) -> *mut F
    {
        let pointer = self.pointer.as_ptr();
        // SAFETY: exclusively owned storage is initialized exactly once.
        unsafe { pointer.write(body) };
        mem::forget(self); // CREATE or the caller's failed-CREATE path owns it.
        pointer
    }

    /// # Safety
    /// The exact capture allocation is initialized and owned by this caller.
    unsafe fn take(state: *mut c_void) -> F
    {
        // SAFETY: entry/destroy exclusivity or failed-CREATE ownership provides
        // this precise initialized allocation and alignment, including ZSTs.
        let allocation = Self { pointer: unsafe { NonNull::new_unchecked(state.cast::<F>()) } };
        let body = unsafe { allocation.pointer.as_ptr().read() };
        drop(allocation);
        body
    }
}
impl<F> Drop for CaptureReservation<F>
{
    fn drop(&mut self)
    {
        let layout = Layout::new::<F>();
        if layout.size() != 0
        {
            // SAFETY: this reservation owns the exact forwarded allocation.
            // Its initialized F, if any, has already been moved out.
            unsafe { ForwardingAllocator.dealloc(self.pointer.as_ptr().cast(), layout) };
        }
    }
}

pub struct PrepareFailure<F>
{
    pub cause: Error,
    pub body:  F
}
pub struct StartFailure
{
    pub cause:       Error,
    pub preparation: PreparedWorker
}

/// Import this extension trait to prepare workers with a checked driver Client.
pub trait Workers
{
    fn prepare_worker<F>(self, name: &str, body: F)
                         -> core::result::Result<PreparedWorker, PrepareFailure<F>>
        where F: FnOnce(WorkerContext) -> Result<()> + Send + 'static;
}
impl Workers for Client
{
    fn prepare_worker<F>(self, name: &str, body: F)
                         -> core::result::Result<PreparedWorker, PrepareFailure<F>>
        where F: FnOnce(WorkerContext) -> Result<()> + Send + 'static
    {
        if name.is_empty()
           || name.len() > transport::MAX_NAME_BYTES
           || name.chars().any(char::is_control)
        {
            return Err(PrepareFailure { cause: Error(BAD_ARGUMENT),
                                        body });
        }
        // Reserve before moving F: every fallible preparation refusal returns
        // its original captures. No global allocator or host fallback is added.
        let reservation = match self.with_module_allocation(CaptureReservation::<F>::try_new)
        {
            Ok(Ok(reservation)) => reservation,
            Ok(Err(cause)) | Err(cause) =>
            {
                return Err(PrepareFailure { cause,
                                            body });
            }
        };
        let state = reservation.write(body);
        let specification = abi::WorkerSpecification { header:
                                                           abi::Header::of::<abi::WorkerSpecification>(),
                                                       entry:           Some(entry::<F>),
                                                       destroy:         Some(destroy::<F>),
                                                       state:           state.cast(),
                                                       state_size:      mem::size_of::<F>(),
                                                       state_alignment: mem::align_of::<F>(),
                                                       name:            name.as_ptr(),
                                                       name_length:     name.len() };
        // SAFETY: both thunks consume precisely this exclusive allocation once.
        // The resident's single publication handoff transfers ownership only on
        // success; the borrowed specification/name remain live through return.
        match unsafe { transport::prepare(self, &specification) }
        {
            Ok(handle) => Ok(PreparedWorker { handle: Some(handle) }),
            Err(cause) =>
            {
                // SAFETY: a failed CREATE never began entry/destroy or took
                // state ownership. This also preserves valid aligned ZST pointers.
                let body = unsafe { CaptureReservation::<F>::take(state.cast()) };
                Err(PrepareFailure { cause,
                                     body })
            }
        }
    }
}

/// An inert or armed preparation is uniquely owned. Starting while module
/// initialization is in progress only arms it; successful registry commit starts
/// it. Failed initialization cannot leave its body running.
pub struct PreparedWorker
{
    handle: Option<WorkerHandle>
}
impl PreparedWorker
{
    pub fn start(mut self) -> core::result::Result<Worker, StartFailure>
    {
        let Some(handle) = self.handle
        else
        {
            return Err(StartFailure { cause:       Error(BAD_ABI),
                                      preparation: self });
        };
        match handle.start()
        {
            Ok(()) =>
            {
                self.handle = None;
                Ok(Worker { handle })
            }
            Err(cause) =>
            {
                Err(StartFailure { cause,
                                   preparation: self })
            }
        }
    }
}
impl Drop for PreparedWorker
{
    fn drop(&mut self)
    {
        if let Some(handle) = self.handle.take()
        {
            handle.dispose_on_drop();
        }
    }
}

pub struct Worker
{
    handle: WorkerHandle
}
impl Worker
{
    pub fn waker(&self) -> WorkerWaker
    {
        WorkerWaker { handle: self.handle }
    }

    pub fn cancel(&self) -> Result<()>
    {
        self.handle.cancel()
    }

    /// Inspect a small result record without waiting. Exited stack/CPU backing
    /// can be reclaimed while this controller still owns the result token.
    pub fn try_join(&self) -> Result<Option<Result<()>>>
    {
        let status = self.handle.status()?;
        if status.state != WorkerState::Finished
        {
            return Ok(None);
        }
        Ok(Some(if status.exit_status == OK { Ok(()) } else { Err(Error(status.exit_status)) }))
    }
}
impl Drop for Worker
{
    fn drop(&mut self)
    {
        self.handle.dispose_on_drop();
    }
}

/// Copied generation-tagged token only. It retains no account, code or stack
/// ownership and is refused once its original worker generation is retired.
#[derive(Clone, Copy)]
pub struct WorkerWaker
{
    handle: WorkerHandle
}
impl WorkerWaker
{
    pub fn wake(self) -> Result<()>
    {
        self.handle.wake()
    }
}

/// Only the entry shim constructs this actual-current-worker capability.
/// It cannot be cloned or transferred to another thread.
pub struct WorkerContext
{
    handle: WorkerHandle,
    thread: PhantomData<*mut ()>
}
impl WorkerContext
{
    pub fn waker(&self) -> WorkerWaker
    {
        WorkerWaker { handle: self.handle }
    }

    pub fn cancelled(&self) -> Result<bool>
    {
        self.handle.cancelled()
    }

    /// Wake-before-park is consumed atomically by the native scheduler. Sticky
    /// cancellation returns OPERATION_CANCELLED instead of sleeping. Parking in
    /// a critical section or with interrupts excluded is refused.
    pub fn park(&self) -> Result<()>
    {
        self.handle.park()
    }

    /// Checked platform monotonic time, available only to this actual worker.
    pub fn monotonic_microseconds(&self) -> Result<u64>
    {
        self.handle.monotonic_microseconds()
    }

    /// Sleep until the duration expires, a retained wake arrives, or sticky
    /// cancellation is observed. Wake and timeout both return Ok; consult the
    /// monotonic schedule for deadlines. Zero returns immediately after checking
    /// cancellation/coalesced wake; it is not a yield guarantee. Positive
    /// fractions round up to microseconds; conversion/deadline overflow refuses.
    /// Timer expiry never patches a remote frame.
    pub fn park_for(&self, duration: core::time::Duration) -> Result<()>
    {
        self.handle.park_for(duration)
    }
}

unsafe extern "C" fn entry<F>(state: *mut c_void) -> i32
    where F: FnOnce(WorkerContext) -> Result<()> + Send + 'static
{
    // SAFETY: CREATE transferred this exact captured allocation; only entry or destroy may run,
    // never both. Move the body onto this actual worker stack and release its
    // allocation before executing it. Its captures are destroyed on return.
    let body = unsafe { CaptureReservation::<F>::take(state) };
    let handle = match transport::current()
    {
        Ok(handle) => handle,
        Err(Error(status)) => return status
    };
    match body(WorkerContext { handle,
                               thread: PhantomData })
    {
        Ok(()) => OK,
        Err(Error(status)) =>
        {
            if status < OK
            {
                status
            }
            else
            {
                BAD_ABI
            }
        }
    }
}
unsafe extern "C" fn destroy<F>(state: *mut c_void)
{
    // SAFETY: the pinned resident calls destroy only for an unstarted state,
    // under its genuine module/resource scope and without manager locks held.
    drop(unsafe { CaptureReservation::<F>::take(state) });
}
