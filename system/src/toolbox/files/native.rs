//! Checked ABI pointer boundary shared by native and host service gates.
use mac_abi::{files::FileCall, *};
/// # Safety
/// The request is a live, exclusively borrowed trap record of the declared size.
#[cfg(target_arch = "riscv64")]
pub(crate) unsafe fn dispatch_async(owner: u64, pointer: *mut (), size: usize) -> Option<i64>
{
    if pointer.is_null()
       || size != size_of::<FileCall>()
       || !(pointer as usize).is_multiple_of(align_of::<FileCall>())
    {
        return None;
    }
    let call = unsafe { &mut *pointer.cast::<FileCall>() };
    if !matches!(call.operation,
                 files::HOST_EXPORT_COUNT | files::MOUNT_HOST_EXPORT | files::UNMOUNT_VOLUME)
    {
        return None;
    }
    if call.cnid != 0
       || call.handle != 0
       || call.fork != 0
       || call.whence != 0
       || call.reserved != 0
       || call.offset != 0
       || call.position != 0
       || call.length != 0
       || call.name_length != 0
       || call.capacity != 0
       || call.operation == files::HOST_EXPORT_COUNT && (call.volume != 0 || call.index != 0)
       || call.operation == files::MOUNT_HOST_EXPORT && call.volume != 0
       || call.operation == files::UNMOUNT_VOLUME && (call.volume == 0 || call.index != 0)
    {
        return Some(BAD_ARGUMENT as i64);
    }
    call.transferred = 0;
    Some(super::provider::runtime::request(owner, call) as i64)
}
/// # Safety
/// Request and nested buffers must be live exclusive borrows for their sizes.
/// `owner` must be captured from trusted execution context before I/O handoff.
pub(crate) unsafe fn dispatch_owned(manager: &mut super::Manager, owner: u64, pointer: *mut (),
                                    size: usize)
                                    -> i64
{
    if size == size_of::<mac_abi::files::DataWriteCall>()
    {
        return unsafe { super::write::dispatch(manager, owner, pointer, size) };
    }
    if size == size_of::<mac_abi::files::FileScanCall>()
    {
        if pointer.is_null()
           || !(pointer as usize).is_multiple_of(align_of::<mac_abi::files::FileScanCall>())
        {
            return BAD_ARGUMENT as i64;
        }
        let call = unsafe { &mut *pointer.cast::<mac_abi::files::FileScanCall>() };
        return manager.scan_request(call).map_or_else(|status| status, |()| OK) as i64;
    }
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
