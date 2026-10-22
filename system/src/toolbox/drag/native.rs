//! The only unsafe boundary: lend validated slices to the safe resident manager.
use mac_abi::{drag::*, *};
/// # Safety
/// Request and declared buffers must be live and exclusively borrowed for this call.
pub unsafe fn dispatch(pointer: *mut (), size: usize,
                       run: impl FnOnce(&mut Call, &[u8], &mut [u8]) -> Result<(), Status>)
                       -> i64
{
    if pointer.is_null()
       || size != size_of::<Call>()
       || !(pointer as usize).is_multiple_of(align_of::<Call>())
    {
        return BAD_ARGUMENT as i64;
    }
    let call = unsafe { &mut *pointer.cast::<Call>() };
    if call.length > MAX_BYTES
       || call.capacity > MAX_BYTES
       || call.length != 0 && call.input.is_null()
       || call.capacity != 0 && call.output.is_null()
    {
        return BAD_ARGUMENT as i64;
    }
    let input = if call.length == 0
    {
        &[]
    }
    else
    {
        unsafe { core::slice::from_raw_parts(call.input, call.length) }
    };
    let output = if call.capacity == 0
    {
        &mut []
    }
    else
    {
        unsafe { core::slice::from_raw_parts_mut(call.output, call.capacity) }
    };
    run(call, input, output).err().unwrap_or(OK) as i64
}
