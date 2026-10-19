//! Borrowed Memory Manager ABI boundary, shared by native ECALLs and host tests.
use mac_abi::{memory::MemoryCall, traps::MEMORY, *};

use super::Manager;
/// # Safety
/// The request and nested slices borrow valid exclusive storage for this call;
/// input, output and request must not alias. Native modules share one address space.
pub unsafe fn dispatch(manager: &mut Manager, number: u32, pointer: *mut (), size: usize) -> i64
{
    if number != MEMORY
    {
        return NOT_SUPPORTED as i64;
    }
    if pointer.is_null()
       || size != core::mem::size_of::<MemoryCall>()
       || !(pointer as usize).is_multiple_of(core::mem::align_of::<MemoryCall>())
    {
        return BAD_ARGUMENT as i64;
    }
    let call = unsafe { &mut *pointer.cast::<MemoryCall>() };
    if call.input_length > 1024 * 1024
       || call.capacity > 1024 * 1024
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
