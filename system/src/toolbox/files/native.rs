//! Checked ABI pointer boundary shared by native and host service gates.
use mac_abi::{files::FileCall, *};
/// # Safety
/// The request is a live, exclusively borrowed trap record of the declared size.
#[cfg(target_arch = "riscv64")]
pub(crate) unsafe fn dispatch_async(owner: u64, pointer: *mut (), size: usize) -> Option<i64>
{
    if size == size_of::<files::FileTransferCall>()
    {
        return Some(unsafe {
            checked_transfer(pointer, size, |call, sources| {
                super::provider::runtime::transfer::request(owner, call, sources) as i64
            })
        });
    }
    if size == size_of::<files::IconGroupCall>()
    {
        return Some(unsafe {
            checked_icon_group(pointer, size, |call, rows| {
                let provider = crate::kernel::with_files(|manager| {
                                   if call.operation == files::BEGIN_ICON_GROUP
                                   {
                                       manager.read_provider(call.volume)
                                   }
                                   else
                                   {
                                       manager.group_provider(owner, call.handle)
                                   }
                               }).map_err(crate::state::AccessError::status)
                                 .and_then(|result| result);
                match provider
                {
                    Ok(Some(_)) =>
                    {
                        super::provider::runtime::request_group(owner, call, rows) as i64
                    }
                    Ok(None) =>
                    {
                        crate::kernel::with_files(|manager| {
                            manager.group_request(owner, call, rows)
                        }).map_err(crate::state::AccessError::status)
                          .and_then(|result| result)
                          .err()
                          .unwrap_or(OK) as i64
                    }
                    Err(error) => error as i64
                }
            })
        });
    }
    if size == size_of::<files::ForkWriteCall>()
    {
        return Some(unsafe {
            super::stream_native::checked(pointer, size, |call, name, comment, data, output| {
                let provider = crate::kernel::with_files(|manager| {
                    if call.operation == files::BEGIN_FORK_WRITE
                    {
                        manager.read_provider(call.volume)
                    }
                    else
                    {
                        manager.stream_provider(owner, call.handle)
                    }
                });
                match provider
                {
                    Ok(Ok(Some(_))) =>
                    {
                        super::provider::runtime::request_stream(owner, call, name, comment, data,
                                                                 output)
                    }
                    Ok(Ok(None)) =>
                    {
                        crate::kernel::with_files(|manager| {
                            manager.stream_request(owner, call, name, comment, data, output)
                        }).map_or_else(|e| e.status(), |r| r.err().unwrap_or(OK))
                    }
                    Ok(Err(error)) => error,
                    Err(error) => error.status()
                }
            })
        });
    }
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
    if matches!(call.operation,
                files::MOVE_TO_TRASH | files::PUT_AWAY | files::EMPTY_TRASH | files::TRASH_USAGE)
    {
        let query = matches!(call.operation, files::EMPTY_TRASH | files::TRASH_USAGE);
        if call.volume == 0
           || (!query && call.cnid <= files::ROOT_ID)
           || query && call.cnid != 0
           || call.index != 0
           || call.handle != 0
           || call.fork != 0
           || call.whence != 0
           || call.reserved != 0
           || call.offset != 0
           || call.length != 0
           || call.name_length != 0
           || call.capacity != 0
        {
            return Some(BAD_ARGUMENT as i64);
        }
        let routed =
            crate::kernel::with_files(|m| {
                let provider = m.read_provider(call.volume)?;
                let info =
                    m.volumes.get(call.volume).and_then(super::Mounted::info).ok_or(NOT_FOUND)?;
                Ok(provider.is_some() || info.filesystem == files::FILESYSTEM_HFS_B)
            }).map_err(crate::state::AccessError::status)
              .and_then(|r| r);
        return match routed
        {
            Ok(true) =>
            {
                Some(super::provider::runtime::transfer::bridge::request(owner, call) as i64)
            }
            Ok(false) => None,
            Err(error) => Some(error as i64)
        };
    }
    if matches!(call.operation,
                files::SET_COMMENT
                | files::SET_LOCKED
                | files::SET_STATIONERY
                | files::SET_ICON_LOCATION
                | files::SET_FOLDER_STATE
                | files::RENAME
                | files::REMOVE
                | files::SET_PROPERTIES
                | files::MOVE_INTO)
    {
        return Some(unsafe { dispatch_metadata_async(owner, call) });
    }
    let catalog = matches!(call.operation,
                           files::CATALOG
                           | files::PARENT
                           | files::DIRECTORY_ENTRY
                           | files::LOOKUP
                           | files::ALLOCATION_INFO
                           | files::COMMENT
                           | files::PATH);
    let trash_query = call.operation == files::TRASH_DIRECTORY;
    let opening = call.operation == files::OPEN;
    let making_folder = call.operation == files::NEW_FOLDER;
    let mounting_file = matches!(call.operation, files::MOUNT_FILE | files::OPEN_ITEM);
    let fork_io = matches!(call.operation, files::READ | files::CLOSE);
    if !matches!(call.operation,
                 files::HOST_EXPORT_COUNT | files::MOUNT_HOST_EXPORT | files::UNMOUNT_VOLUME)
       && !catalog
       && !opening
       && !fork_io
       && !mounting_file
       && !trash_query
       && !making_folder
    {
        return None;
    }
    if (!(catalog || opening || mounting_file || making_folder) && call.cnid != 0)
       || ((catalog || opening || mounting_file || making_folder) && call.cnid == 1)
       || ((catalog || opening || fork_io || mounting_file || making_folder)
           && call.operation != files::DIRECTORY_ENTRY
           && call.index != 0)
       || (fork_io && call.volume != 0)
       || (!fork_io && call.handle != 0)
       || (!opening && call.fork != 0)
       || (opening && !matches!(call.fork, files::DATA_FORK | files::RESOURCE_FORK))
       || call.whence != 0
       || call.reserved != 0
       || call.offset != 0
       || (!(trash_query || making_folder) && call.position != 0)
       || call.length != 0
       || (!matches!(call.operation, files::LOOKUP | files::NEW_FOLDER) && call.name_length != 0)
       || call.name_length > 4096
       || (call.name_length != 0 && call.name.is_null())
       || (!matches!(call.operation, files::READ | files::PATH | files::COMMENT)
           && call.capacity != 0)
       || call.capacity > 1024 * 1024
       || (call.capacity != 0 && call.output.is_null())
       || trash_query && call.index > 1
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
    if catalog || opening || fork_io || trash_query || making_folder
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

/// # Safety
/// The record and its optional input array are live caller borrows. No pointer
/// survives this boundary: admission copies source references into resident state.
#[cfg(target_arch = "riscv64")]
unsafe fn checked_transfer(pointer: *mut (), size: usize,
                           dispatch: impl FnOnce(&mut files::FileTransferCall,
                                  &[files::FileTransferSource])
                                  -> i64)
                           -> i64
{
    if pointer.is_null()
       || size != size_of::<files::FileTransferCall>()
       || !(pointer as usize).is_multiple_of(align_of::<files::FileTransferCall>())
    {
        return BAD_ARGUMENT as i64;
    }
    let call = unsafe { &mut *pointer.cast::<files::FileTransferCall>() };
    if call.operation > files::ABANDON_TRANSFER
       || call.reserved != [0; 4]
       || call.source_count > files::MAX_TRANSFER_SOURCES
       || (call.operation != files::BEGIN_TRANSFER
           && (call.source_count != 0 || !call.sources.is_null()))
       || call.source_count != 0
          && (call.sources.is_null()
              || !(call.sources as usize).is_multiple_of(align_of::<files::FileTransferSource>()))
    {
        return BAD_ARGUMENT as i64;
    }
    let sources = if call.source_count == 0
    {
        &[]
    }
    else
    {
        unsafe { core::slice::from_raw_parts(call.sources, call.source_count) }
    };
    dispatch(call, sources)
}
#[cfg(target_arch = "riscv64")]
unsafe fn dispatch_metadata_async(owner: u64, call: &mut FileCall) -> i64
{
    use mac_abi::files::*;
    let comment = matches!(call.operation, SET_COMMENT | SET_PROPERTIES);
    let rename = call.operation == RENAME;
    let moving = call.operation == MOVE_INTO;
    let removing = call.operation == REMOVE;
    let icon = call.operation == SET_ICON_LOCATION;
    if call.operation == SET_FOLDER_STATE
    {
        let info = call.entry.finder_info;
        let bounds = Rect::from_be_bytes(info[..8].try_into().unwrap());
        if bounds.right <= bounds.left
           || bounds.bottom <= bounds.top
           || info[8..14].iter().chain(info[20..].iter()).any(|b| *b != 0)
           || call.index != 0
        {
            return BAD_ARGUMENT as i64;
        }
    }
    if call.cnid < ROOT_ID
       || call.handle != 0
       || call.reserved != 0
       || call.whence != 0
       || call.offset != 0
       || call.length != 0
       || call.capacity != 0
       || call.name_length > if rename { 4096 } else { 255 }
       || (!(comment || rename) && call.name_length != 0)
       || (rename && (call.name_length == 0 || call.index != 0 || call.cnid <= ROOT_ID))
       || (moving && (call.index < ROOT_ID || call.cnid <= ROOT_ID))
       || (removing && (call.index != 0 || call.cnid <= ROOT_ID))
       || (call.name_length != 0 && call.name.is_null())
       || (!icon && call.fork != 0)
       || (comment && call.index != 0)
       || (matches!(call.operation, SET_LOCKED | SET_STATIONERY) && call.index > 1)
       || (icon
           && (call.fork > 1
               || call.fork == 0 && call.index != 0
               || call.fork == 1 && call.index == u32::MAX))
    {
        return BAD_ARGUMENT as i64;
    }
    let text = if call.name_length == 0
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
    if call.operation == SET_PROPERTIES
    {
        if let Err(error) = super::properties::validate(call, text)
        {
            return error as i64;
        }
    }
    call.transferred = 0;
    if super::provider::runtime::metadata_pending()
    {
        return super::provider::runtime::request_metadata(owner, call, text) as i64;
    }
    let routed = crate::kernel::with_files(|m| {
        if m.read_provider(call.volume)?.is_some()
        {
            Ok(None)
        }
        else
        {
            m.request_owned(owner, call, text, &mut []).map(|()| Some(OK))
        }
    });
    match routed
    {
        Ok(Ok(Some(status))) => status as i64,
        Ok(Ok(None)) => super::provider::runtime::request_metadata(owner, call, text) as i64,
        Ok(Err(error)) => error as i64,
        Err(error) => error.status() as i64
    }
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
/// `owner` and any watch authority must be captured from trusted execution
/// context before I/O handoff; a scalar is insufficient for watch operations.
pub(crate) unsafe fn dispatch_owned(manager: &mut super::Manager, owner: u64, pointer: *mut (),
                                    size: usize, watch: Option<&super::watch::Caller>)
                                    -> i64
{
    if size == size_of::<files::IconGroupCall>()
    {
        return unsafe {
            checked_icon_group(pointer, size, |call, rows| {
                manager.group_request(owner, call, rows).err().unwrap_or(OK) as i64
            })
        };
    }
    if size == size_of::<files::ForkWriteCall>()
    {
        return unsafe { super::stream_native::dispatch(manager, owner, pointer, size) };
    }
    if size == size_of::<files::IconMoveCall>()
    {
        return unsafe { dispatch_icon_moves(manager, owner, pointer, size) };
    }
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
    if matches!(call.operation, files::WATCH_DIRECTORY | files::POLL_WATCH | files::DISPOSE_WATCH)
    {
        let Some(caller) = watch
        else
        {
            return ACCESS_DENIED as i64;
        };
        manager.watch_request(caller, call).map_or_else(i64::from, |()| OK as i64)
    }
    else
    {
        manager.request_owned(owner, call, name, output).map_or_else(|e| e, |()| OK) as i64
    }
}

/// # Safety
/// The request and its declared array are live, aligned caller borrows.
unsafe fn dispatch_icon_moves(manager: &mut super::Manager, owner: u64, pointer: *mut (),
                              size: usize)
                              -> i64
{
    if pointer.is_null()
       || size != size_of::<files::IconMoveCall>()
       || !(pointer as usize).is_multiple_of(align_of::<files::IconMoveCall>())
    {
        return BAD_ARGUMENT as i64;
    }
    let call = unsafe { &mut *pointer.cast::<files::IconMoveCall>() };
    if call.count > 1024
       || call.count != 0
          && (call.entries.is_null()
              || !(call.entries as usize).is_multiple_of(align_of::<files::IconMove>()))
    {
        return BAD_ARGUMENT as i64;
    }
    let rows = if call.count == 0
    {
        &[]
    }
    else
    {
        unsafe { core::slice::from_raw_parts(call.entries, call.count as usize) }
    };
    manager.move_icons(owner, call, rows).err().unwrap_or(OK) as i64
}

/// # Safety
/// The live request and optional begin array are exclusively borrowed for this call.
pub(super) unsafe fn checked_icon_group(pointer: *mut (), size: usize,
                                        dispatch: impl FnOnce(&mut files::IconGroupCall,
                                               &[files::IconMove])
                                               -> i64)
                                        -> i64
{
    if pointer.is_null()
       || size != size_of::<files::IconGroupCall>()
       || !(pointer as usize).is_multiple_of(align_of::<files::IconGroupCall>())
    {
        return BAD_ARGUMENT as i64;
    }
    let call = unsafe { &mut *pointer.cast::<files::IconGroupCall>() };
    if call.reserved != 0
       || call.operation > files::ABANDON_ICON_GROUP
       || call.count > 1024
       || (call.operation != files::BEGIN_ICON_GROUP
           && (call.count != 0 || !call.entries.is_null()))
       || (call.count != 0
           && (call.entries.is_null()
               || !(call.entries as usize).is_multiple_of(align_of::<files::IconMove>())))
    {
        return BAD_ARGUMENT as i64;
    }
    let rows = if call.count == 0
    {
        &[]
    }
    else
    {
        unsafe { core::slice::from_raw_parts(call.entries, call.count as usize) }
    };
    dispatch(call, rows)
}
