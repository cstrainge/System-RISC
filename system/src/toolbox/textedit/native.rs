//! Narrow borrowed UTF-8 pointer boundary; no pointer survives dispatch.
use mac_abi::{textedit::*, traps::TEXTEDIT, *};

use super::Manager;
/// # Safety
/// Request and nested buffers must be valid, non-overlapping borrowed storage
/// for this synchronous call. Address-space isolation awaits the MMU boundary.
pub unsafe fn dispatch(manager: &mut Manager, number: u32, pointer: *mut (), size: usize) -> i64
{
    unsafe {
        dispatch_with(number, pointer, size, |call, input, output| {
            manager.request(call, input, output)
        })
    }
}
/// # Safety
/// Same buffer validity requirements as [`dispatch`].
pub unsafe fn dispatch_with(number: u32, pointer: *mut (), size: usize,
                            request: impl FnOnce(&mut TextEditCall, &str, &mut [u8]) -> Status)
                            -> i64
{
    if number != TEXTEDIT
    {
        return NOT_SUPPORTED as i64;
    }
    if pointer.is_null()
       || size != core::mem::size_of::<TextEditCall>()
       || !(pointer as usize).is_multiple_of(core::mem::align_of::<TextEditCall>())
    {
        return BAD_ARGUMENT as i64;
    }
    let call = unsafe { &mut *pointer.cast::<TextEditCall>() };
    if call.input_length > MAX_BYTES
       || call.capacity > MAX_BYTES
       || (call.input_length != 0 && call.input.is_null())
       || (call.capacity != 0 && call.output.is_null())
    {
        return BAD_ARGUMENT as i64;
    }
    let bytes = if call.input_length == 0
    {
        &[]
    }
    else
    {
        unsafe { core::slice::from_raw_parts(call.input, call.input_length) }
    };
    let Ok(input) = core::str::from_utf8(bytes)
    else
    {
        return BAD_ARGUMENT as i64;
    };
    let output = if call.capacity == 0
    {
        &mut []
    }
    else
    {
        unsafe { core::slice::from_raw_parts_mut(call.output, call.capacity) }
    };
    request(call, input, output) as i64
}
