//! Checked ABI pointer boundary shared by native and host service gates.
use mac_abi::{files::FileCall, *};
/// # Safety
/// The request is a live, exclusively borrowed trap record of the declared size.
#[cfg(target_arch = "riscv64")]
pub(crate) unsafe fn dispatch_async(owner: u64, pointer: *mut (), size: usize) -> Option<i64>
{
    if size == size_of::<files::DataWriteCall>()
    {
        return Some(unsafe {
            super::write::checked(pointer, size, |call, name, data| {
                if call.reserved != [0; 2]
                {
                    return BAD_ARGUMENT;
                }
                let routed = crate::kernel::with_files(|manager| {
                    let asynchronous = if matches!(call.operation,
                                                   files::COMMIT_DATA_WRITE
                                                   | files::ABORT_DATA_WRITE)
                    {
                        match manager.write_provider(owner, call.handle)
                        {
                            Ok(Some(_)) =>
                            {
                                call.operation != files::ABORT_DATA_WRITE
                                || manager.writes
                                          .get(call.handle)
                                          .and_then(|mark| mark.provider.as_ref())
                                          .is_none_or(|state| !state.staged())
                            }
                            Ok(None) => false,
                            Err(error) => return Some(error)
                        }
                    }
                    else
                    {
                        false
                    };
                    if asynchronous
                    {
                        None
                    }
                    else
                    {
                        Some(manager.write_request(owner, call, name, data).err().unwrap_or(OK))
                    }
                });
                match routed
                {
                    Ok(Some(status)) => status,
                    Ok(None) => super::provider::runtime::request_write(owner, call),
                    Err(error) => error.status()
                }
            })
        });
    }
    if size == size_of::<files::FileScanCall>()
    {
        return Some(unsafe { dispatch_scan_async(owner, pointer, size) });
    }
    if pointer.is_null()
       || size != size_of::<FileCall>()
       || !(pointer as usize).is_multiple_of(align_of::<FileCall>())
    {
        return None;
    }
    let call = unsafe { &mut *pointer.cast::<FileCall>() };
    let catalog = matches!(call.operation,
                           files::CATALOG
                           | files::PARENT
                           | files::DIRECTORY_ENTRY
                           | files::LOOKUP
                           | files::ALLOCATION_INFO
                           | files::PATH);
    let opening = call.operation == files::OPEN;
    let fork_io = matches!(call.operation, files::READ | files::CLOSE);
    if !matches!(call.operation,
                 files::HOST_EXPORT_COUNT | files::MOUNT_HOST_EXPORT | files::UNMOUNT_VOLUME)
       && !catalog
       && !opening
       && !fork_io
    {
        return None;
    }
    if (!(catalog || opening) && call.cnid != 0)
       || ((catalog || opening) && call.cnid == 1)
       || ((catalog || opening || fork_io)
           && call.operation != files::DIRECTORY_ENTRY
           && call.index != 0)
       || (fork_io && call.volume != 0)
       || (!fork_io && call.handle != 0)
       || (!opening && call.fork != 0)
       || (opening && !matches!(call.fork, files::DATA_FORK | files::RESOURCE_FORK))
       || call.whence != 0
       || call.reserved != 0
       || call.offset != 0
       || call.position != 0
       || call.length != 0
       || (call.operation != files::LOOKUP && call.name_length != 0)
       || call.name_length > 4096
       || (call.name_length != 0 && call.name.is_null())
       || (!matches!(call.operation, files::READ | files::PATH) && call.capacity != 0)
       || call.capacity > 1024 * 1024
       || (call.capacity != 0 && call.output.is_null())
       || call.operation == files::HOST_EXPORT_COUNT && (call.volume != 0 || call.index != 0)
       || call.operation == files::MOUNT_HOST_EXPORT && call.volume != 0
       || call.operation == files::UNMOUNT_VOLUME && (call.volume == 0 || call.index != 0)
    {
        return Some(BAD_ARGUMENT as i64);
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
            Ok(name) => name,
            Err(_) => return Some(BAD_ARGUMENT as i64)
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
    call.transferred = 0;
    if catalog || opening || fork_io
    {
        // Complete local reads in the same handoff used for routing. Introducing
        // providers must not add a second Files guard/stack switch to disk reads.
        let routed = crate::kernel::with_files(|manager| {
            let provider = if fork_io
            {
                manager.file_provider(owner, call.handle)
            }
            else
            {
                manager.read_provider(call.volume)
            };
            match provider
            {
                Ok(Some(_)) => None,
                Ok(None) =>
                {
                    Some(manager.request_owned(owner, call, name, output)
                                .map_or_else(|status| status, |()| OK))
                }
                Err(error) => Some(error)
            }
        });
        match routed
        {
            Ok(Some(status)) => return Some(status as i64),
            Ok(None) => (),
            Err(error) => return Some(error.status() as i64)
        }
    }
    Some(super::provider::runtime::request(owner, call, name, output) as i64)
}
#[cfg(target_arch = "riscv64")]
unsafe fn dispatch_scan_async(owner: u64, pointer: *mut (), size: usize) -> i64
{
    if pointer.is_null()
       || size != size_of::<files::FileScanCall>()
       || !(pointer as usize).is_multiple_of(align_of::<files::FileScanCall>())
    {
        return BAD_ARGUMENT as i64;
    }
    let call = unsafe { &mut *pointer.cast::<files::FileScanCall>() };
    let routed = crate::kernel::with_files(|manager| {
        match manager.read_provider(call.volume)
        {
            Ok(Some(_)) => None,
            Ok(None) => Some(manager.scan_request(call).map_or_else(|status| status, |()| OK)),
            Err(error) => Some(error)
        }
    });
    match routed
    {
        Ok(Some(status)) => status as i64,
        Ok(None) => super::provider::runtime::request_scan(owner, call) as i64,
        Err(error) => error.status() as i64
    }
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
