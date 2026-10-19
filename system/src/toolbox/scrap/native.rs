//! Borrowed Scrap Manager boundary. User buffers live only for this request;
//! the safe manager copies payloads into bounded resident-owned storage.
use mac_abi::{scrap::{MAX_BYTES, ScrapCall},
              traps::SCRAP,
              *};

use super::Manager;
/// # Safety
/// Request and nested buffers must borrow valid storage for the call. The input,
/// output and request must not alias. The current native ABI shares an address
/// space; pointer isolation requires the future application/MMU boundary.
pub unsafe fn dispatch(manager: &mut Manager, number: u32, pointer: *mut (), size: usize) -> i64
{
    if number != SCRAP
    {
        return NOT_SUPPORTED as i64;
    }
    if pointer.is_null()
       || size != core::mem::size_of::<ScrapCall>()
       || !(pointer as usize).is_multiple_of(core::mem::align_of::<ScrapCall>())
    {
        return BAD_ARGUMENT as i64;
    }
    let call = unsafe { &mut *pointer.cast::<ScrapCall>() };
    if call.input_length > MAX_BYTES
       || call.capacity > MAX_BYTES
       || (call.input_length != 0 && call.input.is_null())
       || (call.capacity != 0 && call.output.is_null())
    {
        return BAD_ARGUMENT as i64;
    }
    let input = if call.input_length == 0
    {
        &[]
    }
    else
    {
        unsafe { core::slice::from_raw_parts(call.input, call.input_length) }
    };
    let output = if call.capacity == 0
    {
        &mut []
    }
    else
    {
        unsafe { core::slice::from_raw_parts_mut(call.output, call.capacity) }
    };
    manager.request(call, input, output) as i64
}
