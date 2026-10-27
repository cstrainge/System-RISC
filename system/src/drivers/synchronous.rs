//! Ticket-free resident broker core shared by the native boundary and host
//! acceptance transport. The caller supplies its platform invocation gate and
//! drops the registry guard before this bounded callback executes. Native SMP
//! admission still requires a concurrent root pin beyond the single-hart gate.
use mac_abi::{BAD_ABI, BAD_ARGUMENT, NOT_SUPPORTED, OK, Status};
use mac_module::abi;

use super::{Registry, jobs::subjects::Subject, registry::requests::Origin, status};

pub(crate) type Callback = fn(u64, u32, &[u8], &mut [u8]) -> Result<usize, Status>;

pub(crate) fn authorize(registry: &Registry, subject: &Subject, caller: u64, provider: u64,
                        call: &abi::Call, callback: impl FnOnce(u64) -> Option<Callback>)
                        -> Result<Callback, Status>
{
    let interface = registry.authorize_synchronous(subject, caller, provider,
                                                   call.input_len as usize,
                                                   call.output_capacity as usize)
                            .map_err(status)?;
    if interface.offer.class.0 != call.class
    {
        return Err(BAD_ARGUMENT);
    }
    callback(provider).ok_or(NOT_SUPPORTED)
}

/// # Safety
/// Call passed checked_call and all declared live, exclusive buffer borrows
/// remain valid until return. Admission and effect share the platform gate.
pub(crate) unsafe fn invoke(call: &mut abi::Call, origin: Origin, caller: u64,
                            admit: impl FnOnce(&abi::Call) -> Result<Callback, Status>)
                            -> Result<Status, Status>
{
    call.output_len = 0;
    if call.phase != abi::SYNCHRONOUS || call.ticket != 0 || origin != Origin::Direct
    {
        return Err(BAD_ARGUMENT);
    }
    // The returned function pointer retains no registry or owner guard.
    let callback = admit(call)?;
    let input = if call.input_len == 0
    {
        &[]
    }
    else
    {
        unsafe { core::slice::from_raw_parts(call.input, call.input_len as usize) }
    };
    let output = if call.output_capacity == 0
    {
        &mut []
    }
    else
    {
        unsafe { core::slice::from_raw_parts_mut(call.output, call.output_capacity as usize) }
    };
    let length = callback(caller, call.operation, input, output)?;
    if length > output.len()
    {
        return Err(BAD_ABI);
    }
    call.output_len = length.try_into().map_err(|_| BAD_ABI)?;
    Ok(OK)
}

/// # Safety
/// Admitted resident or module ABI callers provide live exclusive Call and
/// declared buffer storage. This validates layout, limits and disjoint ranges.
pub(crate) unsafe fn checked_call<'a>(pointer: *mut abi::Call)
                                      -> Result<&'a mut abi::Call, Status>
{
    if pointer.is_null() || !pointer.addr().is_multiple_of(core::mem::align_of::<abi::Call>())
    {
        return Err(BAD_ARGUMENT);
    }
    // Only resident System or already-admitted kernel modules may call broker.
    // Their ABI contract establishes live/exclusive request and buffer borrows.
    let call = unsafe { &mut *pointer };
    if !call.header.valid::<abi::Call>() || call.reserved != 0
    {
        return Err(BAD_ABI);
    }
    if call.input_len > 1024 * 1024
       || call.output_capacity > 1024 * 1024
       || call.input_len != 0 && call.input.is_null()
       || call.output_capacity != 0 && call.output.is_null()
    {
        return Err(BAD_ARGUMENT);
    }
    let input_end = call.input.addr().checked_add(call.input_len as usize).ok_or(BAD_ARGUMENT)?;
    let output_end =
        call.output.addr().checked_add(call.output_capacity as usize).ok_or(BAD_ARGUMENT)?;
    let call_start = pointer.addr();
    let call_end = call_start.checked_add(core::mem::size_of::<abi::Call>()).ok_or(BAD_ARGUMENT)?;
    let overlap = |start: usize, end: usize, other_start: usize, other_end: usize| {
        start < end && other_start < other_end && start < other_end && other_start < end
    };
    if overlap(call.input.addr(), input_end, call.output.addr(), output_end)
       || overlap(call_start, call_end, call.input.addr(), input_end)
       || overlap(call_start, call_end, call.output.addr(), output_end)
    {
        return Err(BAD_ARGUMENT);
    }
    Ok(call)
}
