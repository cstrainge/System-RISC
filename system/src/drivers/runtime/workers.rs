//! Module-owned worker admission. Only this binding reads the C descriptor or
//! calls module entry/drop thunks. The scheduler consumes owned capabilities.
use alloc::string::String;
use core::{alloc::Layout, ffi::c_void, ptr::NonNull};

use mac_abi::{ACCESS_DENIED, BAD_ABI, BAD_ARGUMENT, NO_MEMORY, NOT_FOUND, OK, SERVICE_BUSY,
              Status, WRONG_THREAD};
use mac_module::abi::{self, worker};

use super::{RUNTIME, invocation, status};
use crate::{machine::scheduler, toolbox::handles::OwnerRef};

/// An exclusive capture offered by the SDK. Its unsafe entry contract requires
/// Send + 'static state allocated by the resident forwarding allocator. This
/// wrapper never independently frees that state or invokes a destructor.
struct Capture
{
    state:   NonNull<c_void>,
    entry:   abi::WorkerEntry,
    destroy: abi::WorkerDestroy
}
// SAFETY: the checked native SDK transfers one Send + 'static capture. No
// borrowed descriptor survives admission, and entry/destroy consume it once.
unsafe impl Send for Capture
{
}

/// Before acceptance the SDK still owns capture storage. After publication the
/// registered THRD payload owns this value; run/destroy are explicit and the
/// reaper must retain it with its ticket on a refused invocation. Drop never
/// guesses that it can run privileged foreign code during unwinding/cleanup.
#[must_use = "return an offered capture to its caller, or retain it in the registered worker"]
pub(crate) struct Launch
{
    capture:    Option<Capture>,
    accepted:   bool,
    owner:      OwnerRef,
    allocation: scheduler::accounting::AllocationAuthority,
    execution:  crate::images::ModuleExecution,
    name:       String,
    affinity:   scheduler::ThreadAffinity
}
impl Launch
{
    pub(crate) fn owner(&self) -> &OwnerRef
    {
        &self.owner
    }

    pub(crate) fn allocation(&self) -> &scheduler::accounting::AllocationAuthority
    {
        &self.allocation
    }

    pub(crate) fn execution(&self) -> &crate::images::ModuleExecution
    {
        &self.execution
    }

    pub(crate) fn affinity(&self) -> scheduler::ThreadAffinity
    {
        self.affinity
    }

    pub(crate) fn name(&self) -> &str
    {
        &self.name
    }

    /// Called at the scheduler's unique, irrevocable publication point. No
    /// fallible publication step or CREATE failure follows this transition.
    pub(crate) fn accept(&mut self)
    {
        self.accepted = true;
    }

    /// The failure path has recovered this exclusive, unpublished offer. The
    /// SDK keeps the original allocation and body; no module thunk is called.
    fn return_to_caller(mut self)
    {
        self.capture = None;
    }

    pub(crate) fn run(&mut self) -> Status
    {
        if !self.accepted
        {
            return BAD_ABI;
        }
        if let Err(cause) = crate::context::authenticate_module(self.execution.module())
        {
            return cause;
        }
        let Some(capture) = self.capture.take()
        else
        {
            return BAD_ABI;
        };
        // SAFETY: native preparation retains code and captured state through
        // this actual worker. The SDK entry consumes/destroys its allocation
        // exactly once. The genuine baseline module context is already set.
        let result = unsafe { (capture.entry)(capture.state.as_ptr()) };
        if result > OK { BAD_ABI } else { result }
    }

    /// The reaper installs this thread's real live/pending root allocation
    /// target before calling us. A failed scope entry retains capture/code.
    pub(crate) fn destroy(&mut self) -> Result<(), Status>
    {
        if !self.accepted || self.capture.is_none()
        {
            return Ok(());
        }
        let capture = &mut self.capture;
        invocation::enter(&self.execution, || {
            if let Some(capture) = capture.take()
            {
                // SAFETY: entry never consumed this unique capture. Its
                // destroy thunk and source remain pinned throughout the call.
                unsafe { (capture.destroy)(capture.state.as_ptr()) };
            }
        })
    }
}

/// # Safety
/// The SDK borrows a live, aligned WorkerCall through return. CREATE additionally
/// offers its live descriptor, name and unique forwarded Send capture.
pub(super) unsafe extern "C" fn control(module: u64, pointer: *mut abi::WorkerCall) -> Status
{
    if crate::machine::traps::execution::policy::deny(
        crate::machine::traps::execution::policy::Operation::Worker)
    {
        return ACCESS_DENIED;
    }
    let operation = || -> Result<(), Status> {
        crate::context::authenticate_module(module)?;
        if pointer.is_null() || !pointer.is_aligned()
        {
            return Err(BAD_ARGUMENT);
        }
        // SAFETY: the audited SDK supplies an exclusive call of this exact type.
        let call = unsafe { &mut *pointer };
        if !call.header.valid::<abi::WorkerCall>() || call.flags != 0 || call.microseconds != 0
        {
            return Err(BAD_ABI);
        }
        call.state = worker::PREPARED;
        call.exit_status = OK;
        if call.operation == worker::CREATE
        {
            if call.worker != 0 || call.specification.is_null() || !call.specification.is_aligned()
            {
                return Err(BAD_ARGUMENT);
            }
            // SAFETY: CREATE's descriptor/name remain borrowed through return;
            // storage ownership transfers only if publication returns success.
            call.worker = unsafe { create(module, call.specification) }?;
            return Ok(());
        }
        if !call.specification.is_null()
        {
            return Err(BAD_ARGUMENT);
        }
        if call.operation == worker::CURRENT
        {
            if call.worker != 0
            {
                return Err(BAD_ARGUMENT);
            }
            let (id, cancelled) = scheduler::worker_current()?;
            call.worker = id;
            call.flags = if cancelled { worker::CANCEL_REQUESTED } else { 0 };
            return Ok(());
        }
        if call.worker == 0
        {
            return Err(BAD_ARGUMENT);
        }
        // Authenticated cleanup remains valid after ordinary admission stops.
        // Bounded controls need no registry mutation, allocation or callback.
        let owner = crate::context::module_owner(module)?;
        match call.operation
        {
            worker::START =>
            {
                if crate::context::critical_active()
                {
                    return Err(WRONG_THREAD);
                }
                // Registry -> scheduler is the publication lock order. No
                // callback or allocation occurs in this armed/ready transition.
                RUNTIME.with(|runtime| {
                           let phase = runtime.registry.state(module).map_err(status)?;
                           match phase
                           {
                               super::super::registry::State::Initializing =>
                               {
                                   scheduler::worker_start(&owner, call.worker, false)
                               }
                               super::super::registry::State::Ready =>
                               {
                                   scheduler::worker_start(&owner, call.worker, true)
                               }
                               _ => Err(SERVICE_BUSY)
                           }
                       })
                       .map_err(crate::state::AccessError::status)??;
            }
            worker::WAKE => scheduler::worker_wake(&owner, call.worker)?,
            worker::CANCEL => scheduler::worker_cancel(&owner, call.worker)?,
            worker::DISPOSE => scheduler::worker_dispose(&owner, call.worker)?,
            worker::STATUS =>
            {
                let snapshot = scheduler::worker_status(&owner, call.worker)?;
                call.state = snapshot.state;
                call.exit_status = snapshot.exit_status;
                call.flags = if snapshot.cancelled { worker::CANCEL_REQUESTED } else { 0 };
            }
            worker::PARK | worker::EXIT | worker::MONOTONIC | worker::PARK_FOR =>
            {
                return Err(WRONG_THREAD);
            }
            _ => return Err(BAD_ARGUMENT)
        }
        Ok(())
    };
    operation().map_or_else(core::convert::identity, |()| OK)
}

unsafe fn create(module: u64, pointer: *const abi::WorkerSpecification) -> Result<u64, Status>
{
    if crate::context::critical_active()
    {
        return Err(WRONG_THREAD);
    }
    // SAFETY: control checked alignment and the SDK retains a live header.
    // Check the exact descriptor size before reading its expanded fields.
    let header = unsafe { pointer.cast::<abi::Header>().read() };
    if !header.valid::<abi::WorkerSpecification>()
    {
        return Err(BAD_ABI);
    }
    // SAFETY: the exact current descriptor remains borrowed through CREATE.
    let specification = unsafe { *pointer };
    if specification.reserved != 0
    {
        return Err(BAD_ARGUMENT);
    }
    let affinity = match specification.affinity
    {
        worker::AFFINITY_AUTOMATIC => scheduler::ThreadAffinity::Eligible,
        worker::AFFINITY_PRIMARY => scheduler::ThreadAffinity::Primary,
        _ => return Err(BAD_ARGUMENT)
    };
    let entry = specification.entry.ok_or(BAD_ARGUMENT)?;
    let destroy = specification.destroy.ok_or(BAD_ARGUMENT)?;
    invocation::callback_allowed(module, entry as usize)?;
    invocation::callback_allowed(module, destroy as usize)?;
    let layout = Layout::from_size_align(specification.state_size, specification.state_alignment)
        .map_err(|_| BAD_ARGUMENT)?;
    let state = NonNull::new(specification.state).ok_or(BAD_ARGUMENT)?;
    if !state.as_ptr().addr().is_multiple_of(layout.align())
       || state.as_ptr().addr().checked_add(layout.size()).is_none()
       || specification.name.is_null()
       || specification.name_length == 0
       || specification.name_length > mac_module::MAX_NAME_BYTES
       || specification.name.addr().checked_add(specification.name_length).is_none()
    {
        return Err(BAD_ARGUMENT);
    }
    // SAFETY: the SDK retains this immutable name borrow through CREATE return.
    let bytes =
        unsafe { core::slice::from_raw_parts(specification.name, specification.name_length) };
    let text = core::str::from_utf8(bytes).map_err(|_| BAD_ARGUMENT)?;
    if text.chars().any(char::is_control)
    {
        return Err(BAD_ARGUMENT);
    }
    let (owner, allocation, image) =
        RUNTIME.with(|runtime| {
                   if !matches!(runtime.registry.state(module).map_err(status)?,
                                super::super::registry::State::Initializing
                                | super::super::registry::State::Ready)
                   {
                       return Err(SERVICE_BUSY);
                   }
                   let owner = runtime.registry.module_ledger(module).map_err(status)?;
                   let allocation = runtime.registry.module_allocation(module).map_err(status)?;
                   let binding =
                       runtime.bindings.iter().find(|b| b.module == module).ok_or(NOT_FOUND)?;
                   Ok((owner, allocation, binding.image))
               })
               .map_err(crate::state::AccessError::status)??;
    let execution = crate::kernel::module_execution(image)?;
    if execution.module() != owner.owner().incarnation()
    {
        return Err(ACCESS_DENIED);
    }
    let launch = crate::allocation::with_kernel_allocation(allocation.clone(),
                                                           || -> Result<Launch, Status> {
                                                               let mut name = String::new();
                                                               name.try_reserve_exact(text.len())
                                                                   .map_err(|_| NO_MEMORY)?;
                                                               name.push_str(text);
                                                               Ok(Launch { capture:
                                                                               Some(Capture { state,
                                                                                              entry,
                                                                                              destroy }),
                                                                           accepted: false,
                                                                           owner,
                                                                           allocation,
                                                                           execution,
                                                                           name,
                                                                           affinity })
                                                           })??;
    let prepared = match scheduler::prepare_worker(launch)
    {
        Ok(prepared) => prepared,
        Err(failure) =>
        {
            failure.launch.return_to_caller();
            return Err(failure.status);
        }
    };
    match scheduler::publish_worker(prepared)
    {
        Ok(id) => Ok(id),
        Err(failure) =>
        {
            match failure.prepared.return_launch()
            {
                Ok(launch) => launch.return_to_caller(),
                Err(prepared) =>
                {
                    // Publication refusal has not accepted the capture. The
                    // absent offer cannot authorize a foreign drop callback.
                    serial!("MODULE_WORKER_OFFER_MISSING module={} cause={}; capture remains \
                             with its original SDK caller",
                            module,
                            failure.status);
                    drop(prepared);
                    return Err(BAD_ABI);
                }
            }
            Err(failure.status)
        }
    }
}
