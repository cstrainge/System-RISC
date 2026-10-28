//! Independently linked module allocator boundary. The installed invocation,
//! never the Host's copied context number, authenticates each callback. Backing
//! and original-account receipts remain exclusively in the resident allocator.
use alloc::alloc::{alloc, dealloc, realloc};
use core::{alloc::Layout, ffi::c_void, ptr};

use mac_abi::{BAD_ABI, BAD_ARGUMENT, NO_MEMORY, OK, Status};

fn layout(size: usize, alignment: usize) -> Result<Layout, Status>
{
    if size == 0
    {
        return Err(BAD_ARGUMENT);
    }
    Layout::from_size_align(size, alignment).map_err(|_| BAD_ARGUMENT)
}

fn output(pointer: *mut *mut u8) -> Result<(), Status>
{
    if pointer.is_null() || !pointer.is_aligned() { Err(BAD_ARGUMENT) } else { Ok(()) }
}

/// # Safety
/// The pinned SDK supplies an exclusive live output slot. Request storage never
/// crosses the callback, and allocation failure transfers no ownership.
pub(super) unsafe extern "C" fn allocate(module: u64, size: usize, alignment: usize,
                                         result: *mut *mut u8)
                                         -> Status
{
    let operation = || -> Result<(), Status> {
        crate::context::authenticate_module(module)?;
        output(result)?;
        // SAFETY: the caller supplies the checked exclusive output slot.
        unsafe { result.write(ptr::null_mut()) };
        let requested = layout(size, alignment)?;
        // SAFETY: this validated nonzero layout uses the resident global
        // allocator under the original current Subject's allocation selection.
        let pointer = unsafe { alloc(requested) };
        if pointer.is_null()
        {
            return Err(NO_MEMORY);
        }
        // SAFETY: ownership transfers only through this exclusive output slot.
        unsafe { result.write(pointer) };
        Ok(())
    };
    operation().map_or_else(core::convert::identity, |()| OK)
}

/// # Safety
/// Pointer is an exclusive live allocation returned through this forwarding
/// boundary, with its exact current Layout. The resident header retains the
/// original refund destination even when another subject frees the block.
pub(super) unsafe extern "C" fn deallocate(module: u64, pointer: *mut u8, size: usize,
                                           alignment: usize)
                                           -> Status
{
    let operation = || -> Result<(), Status> {
        crate::context::authenticate_module(module)?;
        let requested = layout(size, alignment)?;
        if pointer.is_null() || !pointer.addr().is_multiple_of(alignment)
        {
            return Err(BAD_ARGUMENT);
        }
        // SAFETY: the SDK's GlobalAlloc contract supplies this live allocation
        // and matching layout. Original receipt destruction follows backing
        // disposal after releasing the resident free-list lock.
        unsafe { dealloc(pointer, requested) };
        Ok(())
    };
    operation().map_or_else(core::convert::identity, |()| OK)
}

/// # Safety
/// Pointer and old Layout satisfy GlobalAlloc::realloc. Result is an exclusive
/// live output slot. Failure leaves the original block and its receipt intact.
pub(super) unsafe extern "C" fn reallocate(module: u64, pointer: *mut u8, size: usize,
                                           alignment: usize, replacement_size: usize,
                                           result: *mut *mut u8)
                                           -> Status
{
    let operation = || -> Result<(), Status> {
        crate::context::authenticate_module(module)?;
        output(result)?;
        // SAFETY: caller supplies the checked exclusive output slot.
        unsafe { result.write(ptr::null_mut()) };
        let requested = layout(size, alignment)?;
        let _replacement = layout(replacement_size, alignment)?;
        if pointer.is_null() || !pointer.addr().is_multiple_of(alignment)
        {
            return Err(BAD_ARGUMENT);
        }
        // SAFETY: the exact live allocation/layout is supplied by the pinned
        // forwarding SDK; replacement keeps its original account and class.
        let replacement = unsafe { realloc(pointer, requested, replacement_size) };
        if replacement.is_null()
        {
            return Err(NO_MEMORY);
        }
        // SAFETY: only successful replacement transfers the new allocation.
        unsafe { result.write(replacement) };
        Ok(())
    };
    operation().map_or_else(core::convert::identity, |()| OK)
}

/// # Safety
/// Callback is a synchronous C thunk in this module's admitted executable
/// range. State points to its live stack record and remains exclusive until
/// return. No Rust trait layout or borrowed result escapes through the wire.
pub(super) unsafe extern "C" fn with_module_allocation(module: u64,
                                                       callback: unsafe extern "C" fn(*mut c_void)
                                                                            -> Status,
                                                       state: *mut c_void)
                                                       -> Status
{
    let operation = || -> Result<Status, Status> {
        crate::context::authenticate_module(module)?;
        super::invocation::callback_allowed(module, callback as usize)?;
        let account = super::module_allocation(module)?;
        crate::allocation::with_kernel_allocation(account, || {
            // SAFETY: callback address is validated against the actual pinned
            // module; its SDK owns the stack record throughout this call.
            let status = unsafe { callback(state) };
            if status > OK { BAD_ABI } else { status }
        })
    };
    operation().unwrap_or_else(core::convert::identity)
}
