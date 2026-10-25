//! Narrow checked pointer boundary; streamed-write policy and storage are safe Rust.
use super::*;
/// # Safety
/// The request and its declared ranges are live and nonaliasing for this trap.
pub(super) unsafe fn dispatch(manager: &mut Manager, owner: u64,
                              authority: Option<&crate::toolbox::handles::OwnerRef>,
                              pointer: *mut (), size: usize)
                              -> i64
{
    unsafe {
        checked(pointer, size, |call, name, comment, data, output| {
            let result = if call.operation == BEGIN_FORK_WRITE
            {
                authority.ok_or(ACCESS_DENIED).and_then(|authority| {
                                                  manager.stream_request_for(owner, authority,
                                                                             call, name, comment,
                                                                             data, output)
                                              })
            }
            else
            {
                manager.stream_request(owner, call, name, comment, data, output)
            };
            result.err().unwrap_or(OK)
        })
    }
}
/// # Safety
/// Same pointer and range contract as dispatch; the callback cannot retain borrows.
pub(super) unsafe fn checked(pointer: *mut (), size: usize,
                             request: impl FnOnce(&mut ForkWriteCall,
                                    &str,
                                    &str,
                                    &[u8],
                                    &mut [u8])
                                    -> Status)
                             -> i64
{
    if pointer.is_null()
       || size != size_of::<ForkWriteCall>()
       || !(pointer as usize).is_multiple_of(align_of::<ForkWriteCall>())
    {
        return BAD_ARGUMENT as i64;
    }
    let call = unsafe { &mut *pointer.cast::<ForkWriteCall>() };
    if call.name_length > 768
       || call.comment_length > 255
       || call.count > 64 * 1024
       || call.capacity > 64 * 1024
       || call.name_length != 0 && call.name.is_null()
       || call.comment_length != 0 && call.comment.is_null()
       || call.count != 0 && call.data.is_null()
       || call.capacity != 0 && call.output.is_null()
       || call.padding != 0
       || call.reserved != 0
       || call.backup_present > 1
       || call.backup_present == 0 && call.backup != 0
       || call.fork > RESOURCE_FORK
    {
        return BAD_ARGUMENT as i64;
    }
    let name = if call.name_length == 0
    {
        &[]
    }
    else
    {
        unsafe { core::slice::from_raw_parts(call.name, call.name_length) }
    };
    let comment = if call.comment_length == 0
    {
        &[]
    }
    else
    {
        unsafe { core::slice::from_raw_parts(call.comment, call.comment_length) }
    };
    let (Ok(name), Ok(comment)) = (core::str::from_utf8(name), core::str::from_utf8(comment))
    else
    {
        return BAD_ARGUMENT as i64;
    };
    let data = if call.count == 0
    {
        &[]
    }
    else
    {
        unsafe { core::slice::from_raw_parts(call.data, call.count) }
    };
    let output = if call.capacity == 0
    {
        &mut []
    }
    else
    {
        unsafe { core::slice::from_raw_parts_mut(call.output, call.capacity) }
    };
    request(call, name, comment, data, output) as i64
}
