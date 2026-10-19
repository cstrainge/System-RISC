//! Checked ABI pointer boundary shared by native and host service gates.
use mac_abi::{files::FileCall, *};
/// # Safety
/// Request and nested buffers must be live exclusive borrows for their sizes.
/// `owner` must be captured from trusted execution context before I/O handoff.
pub(crate) unsafe fn dispatch_owned(manager: &mut super::Manager, owner: u64, pointer: *mut (),
                                    size: usize)
                                    -> i64
{
    if pointer.is_null()
       || size != size_of::<FileCall>()
       || !(pointer as usize).is_multiple_of(align_of::<FileCall>())
    {
        return BAD_ARGUMENT as i64;
    }
    let call = unsafe { &mut *pointer.cast::<FileCall>() };
    if call.name_length > 4096
       || call.capacity > 1024 * 1024
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
            Ok(text) => text,
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
    manager.request_owned(owner, call, name, output).map_or_else(|e| e, |()| OK) as i64
}
