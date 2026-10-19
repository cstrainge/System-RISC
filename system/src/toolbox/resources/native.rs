//! Resource text/buffer validation shared by both service gates.
use mac_abi::{resources::ResourceCall, *};
/// # Safety
/// Call and nested buffers must be live and exclusive for their declared sizes.
pub(crate) unsafe fn dispatch(manager: super::manager::Manager<'_>, pointer: *mut (), size: usize)
                              -> i64
{
    if pointer.is_null()
       || size != size_of::<ResourceCall>()
       || !(pointer as usize).is_multiple_of(align_of::<ResourceCall>())
    {
        return BAD_ARGUMENT as i64;
    }
    let call = unsafe { &mut *pointer.cast::<ResourceCall>() };
    if call.name_length > 65535
       || call.capacity > 4 * 1024 * 1024
       || (call.name_length != 0 && call.name.is_null())
       || (call.capacity != 0 && call.output.is_null())
    {
        return BAD_ARGUMENT as i64;
    }
    let name = if call.name_length == 0
    {
        ""
    }
    else
    {
        match core::str::from_utf8(unsafe {
                  core::slice::from_raw_parts(call.name, call.name_length)
              })
        {
            Ok(s) => s,
            Err(_) => return BAD_ARGUMENT as i64
        }
    };
    let output = if call.capacity == 0
    {
        &mut [][..]
    }
    else
    {
        unsafe { core::slice::from_raw_parts_mut(call.output, call.capacity) }
    };
    manager.request(call, name, output).map_or_else(|e| e, |()| OK) as i64
}
