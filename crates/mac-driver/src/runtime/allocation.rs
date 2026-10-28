//! Forward allocations through the immutable resident descriptor installed at
//! entry. There is no private heap or allocation before admission. The resident
//! retains the original account and backend layout across free/reallocation.
use core::{alloc::{GlobalAlloc, Layout},
           ffi::c_void,
           ptr,
           sync::atomic::{AtomicPtr, Ordering}};

use mac_abi::{BAD_ABI, NOT_SUPPORTED, OK};

use crate::{Error, Result, abi};

static HOST: AtomicPtr<abi::Host> = AtomicPtr::new(ptr::null_mut());

/// # Safety
/// Entry has checked all callbacks. The descriptor stays immutable and alive
/// until every allocation is freed, including after initialization failure.
pub(crate) unsafe fn install(host: *const abi::Host) -> Result<()>
{
    match HOST.compare_exchange(ptr::null_mut(),
                                host.cast_mut(),
                                Ordering::AcqRel,
                                Ordering::Acquire)
    {
        Ok(_) => Ok(()),
        Err(previous) if ptr::eq(previous, host) => Ok(()),
        Err(_) => Err(Error(BAD_ABI))
    }
}

fn host() -> Option<&'static abi::Host>
{
    // SAFETY: install accepts only an immutable resident lifetime descriptor.
    unsafe { HOST.load(Ordering::Acquire).as_ref() }
}

/// Run synchronous construction/growth of retained module storage. This uses
/// the same installed descriptor as the allocator; an uninstalled runtime
/// refuses the operation without running it. Never wrap an asynchronous poll,
/// dependency invocation or whole request in this scope.
pub fn with_module_allocation<R>(operation: impl FnOnce() -> R) -> Result<R>
{
    with_host(host().ok_or(Error(NOT_SUPPORTED))?, operation)
}

pub(crate) fn with_host<R, F: FnOnce() -> R>(host: &abi::Host, operation: F) -> Result<R>
{
    struct State<F, R>
    {
        operation: Option<F>,
        result:    Option<R>
    }
    unsafe extern "C" fn invoke<F: FnOnce() -> R, R>(pointer: *mut c_void) -> i32
    {
        // SAFETY: the synchronous host contract borrows this exact stack state
        // exclusively and retains neither it nor the monomorphized callback.
        let state = unsafe { &mut *pointer.cast::<State<F, R>>() };
        let Some(operation) = state.operation.take()
        else
        {
            return BAD_ABI;
        };
        state.result = Some(operation());
        OK
    }
    let callback = host.with_module_allocation.ok_or(Error(NOT_SUPPORTED))?;
    let mut state = State { operation: Some(operation),
                            result:    None };
    // SAFETY: callback is validated at entry. All Rust values remain on our
    // stack; only an opaque pointer and C function pointer cross the ABI.
    let status =
        unsafe { callback(host.context, invoke::<F, R>, ptr::from_mut(&mut state).cast()) };
    if status != OK
    {
        return Err(Error(if status < 0 { status } else { BAD_ABI }));
    }
    if state.operation.is_some()
    {
        return Err(Error(BAD_ABI));
    }
    state.result.take().ok_or(Error(BAD_ABI))
}

/// Module global allocator. Allocation errors return null; a failed realloc
/// preserves the old allocation. Disposal/protocol errors are terminal and loud
/// because continuing would silently lose an original-account receipt.
pub struct ForwardingAllocator;

fn checked(operation: &str, status: i32, pointer: *mut u8, size: usize, align: usize) -> *mut u8
{
    // Only the resident's explicit allocation refusal is an ordinary null
    // result. Authentication, argument and ABI failures retain their actual
    // status in the terminal protocol diagnostic rather than becoming OOM.
    if status == mac_abi::NO_MEMORY && pointer.is_null()
    {
        return ptr::null_mut();
    }
    if status == OK && !pointer.is_null() && pointer.addr().is_multiple_of(align)
    {
        return pointer;
    }
    super::allocation_fault(format_args!("operation={operation} status={status} \
                                          returned={pointer:p} bytes={size} alignment={align}"))
}

// SAFETY: resident callbacks implement the global allocator ownership/layout
// contract and synchronize their real heap. No Rust allocator state lives here.
unsafe impl GlobalAlloc for ForwardingAllocator
{
    unsafe fn alloc(&self, layout: Layout) -> *mut u8
    {
        let Some(host) = host()
        else
        {
            return ptr::null_mut();
        };
        let Some(allocate) = host.allocate
        else
        {
            return ptr::null_mut();
        };
        let mut output = ptr::null_mut();
        let status = unsafe { allocate(host.context, layout.size(), layout.align(), &mut output) };
        checked("allocate", status, output, layout.size(), layout.align())
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout)
    {
        let Some(host) = host()
        else
        {
            super::allocation_fault(format_args!("operation=deallocate cause=host-not-installed \
                                                  pointer={pointer:p} bytes={} alignment={}",
                                                 layout.size(),
                                                 layout.align()))
        };
        let Some(deallocate) = host.deallocate
        else
        {
            super::allocation_fault(format_args!("operation=deallocate cause=callback-missing \
                                                  module={} pointer={pointer:p} bytes={} \
                                                  alignment={}",
                                                 host.module,
                                                 layout.size(),
                                                 layout.align()))
        };
        let status = unsafe { deallocate(host.context, pointer, layout.size(), layout.align()) };
        if status != OK
        {
            super::allocation_fault(format_args!("operation=deallocate status={status} \
                                                  module={} pointer={pointer:p} bytes={} \
                                                  alignment={}",
                                                 host.module,
                                                 layout.size(),
                                                 layout.align()));
        }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8
    {
        let Some(host) = host()
        else
        {
            super::allocation_fault(format_args!("operation=reallocate cause=host-not-installed \
                                                  pointer={pointer:p} old_bytes={} alignment={} \
                                                  new_bytes={size}",
                                                 layout.size(),
                                                 layout.align()))
        };
        let Some(reallocate) = host.reallocate
        else
        {
            super::allocation_fault(format_args!("operation=reallocate cause=callback-missing \
                                                  module={} pointer={pointer:p} old_bytes={} \
                                                  alignment={} new_bytes={size}",
                                                 host.module,
                                                 layout.size(),
                                                 layout.align()))
        };
        let mut output = ptr::null_mut();
        let status = unsafe {
            reallocate(host.context, pointer, layout.size(), layout.align(), size, &mut output)
        };
        checked("reallocate", status, output, size, layout.align())
    }
}
