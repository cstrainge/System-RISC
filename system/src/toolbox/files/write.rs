//! Owned bounded data-file transactions. Payload is staged fallibly, then the
//! block filesystem or an owned provider job publishes it. Existing resource
//! forks are preserved; no direct resource fork writer is exposed.
#![cfg_attr(not(any(target_arch = "riscv64", feature = "test-support")), allow(dead_code))]
use super::*;
pub(super) struct Pending
{
    pub owner:         u64,
    pub volume:        u32,
    pub parent:        u32,
    pub expected_cnid: u32,
    pub file_type:     [u8; 4],
    pub creator:       [u8; 4],
    pub when:          u64,
    pub name:          Vec<u8>,
    pub comparison:    Vec<u8>,
    pub length:        usize,
    pub data:          Vec<u8>,
    pub provider:      Option<provider::writes::State>,
    pub directory:     bool
}
impl Pending
{
    pub(super) fn orphaned(&self) -> bool
    {
        self.provider.as_ref().is_some_and(|state| state.orphaned())
    }
}
pub(super) fn status(error: MutationError) -> Status
{
    match error
    {
        MutationError::Invalid => BAD_ARGUMENT,
        MutationError::ReadOnly => READ_ONLY,
        MutationError::NotFound => NOT_FOUND,
        MutationError::Exists => ALREADY_EXISTS,
        MutationError::Protected => ACCESS_DENIED,
        MutationError::Busy => SERVICE_BUSY,
        MutationError::Full => DISK_FULL,
        MutationError::NoMemory => NO_MEMORY,
        MutationError::Unsupported => NOT_SUPPORTED,
        MutationError::Io => IO_ERROR,
        MutationError::RollbackFailed => ROLLBACK_FAILED
    }
}
impl Manager
{
    pub(crate) fn write_request(&mut self, owner: u64, call: &mut DataWriteCall, name: &str,
                                data: &[u8])
                                -> Result<(), Status>
    {
        if call.reserved != [0; 2]
        {
            return Err(BAD_ARGUMENT);
        }
        if call.operation == BEGIN_DATA_WRITE
        {
            if call.length > MAX_DATA_WRITE
            {
                return Err(NO_MEMORY);
            }
            if call.expected_cnid != 0
               && self.document_pins
                      .iter()
                      .any(|(_, volume, id)| *volume == call.volume && *id == call.expected_cnid)
            {
                return Err(SERVICE_BUSY);
            }
            self.validate_provider_mutation(call.volume)?;
            if let Some(provider) = self.read_provider(call.volume)?
            {
                return self.reserve_provider_write(owner, call, name, provider);
            }
            let format = match self.mounted(call.volume)?.info.filesystem
            {
                FILESYSTEM_HFS => mac_filesystem::Format::Hfs,
                FILESYSTEM_HFS_B => mac_filesystem::Format::HfsB,
                _ => return Err(BAD_ABI)
            };
            let (name, comparison) = format.prepare_name(name).map_err(|_| BAD_ARGUMENT)?;
            let parent = if call.parent == 0 { ROOT_ID } else { call.parent };
            let directory = self.entry(call.volume, parent)?;
            if !directory.directory
            {
                return Err(BAD_ARGUMENT);
            }
            if self.mounted(call.volume)?.info.read_only != 0
            {
                return Err(READ_ONLY);
            }
            if self.writes.iter().any(|(_, p)| {
                                     p.volume == call.volume
                                     && p.parent == parent
                                     && format.keys_equal(&p.comparison, &comparison)
                                 })
            {
                return Err(SERVICE_BUSY);
            }
            self.writes.reserve().map_err(|_| NO_MEMORY)?;
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(call.length).map_err(|_| NO_MEMORY)?;
            call.handle = self.writes
                              .insert(Pending { owner,
                                                volume: call.volume,
                                                parent,
                                                expected_cnid: call.expected_cnid,
                                                file_type: call.file_type,
                                                creator: call.creator,
                                                when: call.when,
                                                name,
                                                comparison,
                                                length: call.length,
                                                data: bytes,
                                                provider: None,
                                                directory: false })
                              .map_err(|_| NO_MEMORY)?;
            return Ok(());
        }
        let pending = self.writes.get_mut(call.handle).ok_or(NOT_FOUND)?;
        if pending.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        if pending.provider.as_ref().is_some_and(|state| !state.staged())
        {
            return Err(SERVICE_BUSY);
        }
        match call.operation
        {
            APPEND_DATA_WRITE =>
            {
                if pending.data.len().checked_add(data.len()).is_none_or(|n| n > pending.length)
                {
                    return Err(BAD_ARGUMENT);
                }
                pending.data.extend_from_slice(data);
                Ok(())
            }
            ABORT_DATA_WRITE =>
            {
                self.writes.remove(call.handle).ok_or(NOT_FOUND)?;
                Ok(())
            }
            COMMIT_DATA_WRITE =>
            {
                if pending.data.len() != pending.length
                {
                    return Err(BAD_ARGUMENT);
                }
                if pending.provider.is_some()
                {
                    return Err(NOT_SUPPORTED);
                }
                let pending = self.writes.remove(call.handle).ok_or(NOT_FOUND)?;
                let mut protected = Vec::new();
                protected.try_reserve_exact(MAX_OPEN_FILES).map_err(|_| NO_MEMORY)?;
                for (_, file) in self.handles.iter()
                {
                    if file.volume == pending.volume
                    {
                        protected.push(file.id);
                    }
                }
                let mounted = self.mounted(pending.volume)?;
                if mounted.poisoned
                {
                    return Err(ROLLBACK_FAILED);
                }
                mounted.catalog_revision = scan::next_revision()?;
                let mut volume = io(Volume::mount(&mut mounted.device, mounted.partition))?;
                let result =
                    volume.save_data(mac_filesystem::DataSave { parent:    pending.parent,
                                                                name:      &pending.name,
                                                                expected:  pending.expected_cnid,
                                                                file_type: pending.file_type,
                                                                creator:   pending.creator,
                                                                when:      pending.when },
                                     &pending.data,
                                     &protected);
                mounted.scanner = volume.catalog_scanner().ok();
                match result
                {
                    Ok(entry) =>
                    {
                        mounted.info.free_bytes = volume.free_bytes;
                        mounted.info.modified = volume.modified;
                        call.entry = metadata(pending.volume, &entry);
                        self.notify_directory(pending.volume, pending.parent)?;
                        Ok(())
                    }
                    Err(error) =>
                    {
                        if error == MutationError::RollbackFailed
                        {
                            mounted.poisoned = true;
                            mounted.info.read_only = 1;
                        }
                        Err(status(error))
                    }
                }
            }
            _ => Err(NOT_SUPPORTED)
        }
    }
}
/// # Safety
/// All declared borrowed request/name/data ranges must be valid for this call.
pub(super) unsafe fn dispatch(manager: &mut Manager, owner: u64, pointer: *mut (), size: usize)
                              -> i64
{
    unsafe {
        checked(pointer, size, |call, name, data| {
            manager.write_request(owner, call, name, data).err().unwrap_or(OK)
        })
    }
}
/// # Safety
/// All declared request/name/data ranges must be valid for this call. The
/// callback must not retain these borrows when returning a park request.
pub(super) unsafe fn checked(pointer: *mut (), size: usize,
                             request: impl FnOnce(&mut DataWriteCall, &str, &[u8]) -> Status)
                             -> i64
{
    if pointer.is_null()
       || size != size_of::<DataWriteCall>()
       || !(pointer as usize).is_multiple_of(align_of::<DataWriteCall>())
    {
        return BAD_ARGUMENT as i64;
    }
    let call = unsafe { &mut *pointer.cast::<DataWriteCall>() };
    if call.name_length > 768
       || call.data_length > 64 * 1024
       || call.name_length != 0 && call.name.is_null()
       || call.data_length != 0 && call.data.is_null()
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
            Ok(name) => name,
            Err(_) => return BAD_ARGUMENT as i64
        }
    };
    let data = if call.data_length == 0
    {
        &[]
    }
    else
    {
        unsafe { core::slice::from_raw_parts(call.data, call.data_length) }
    };
    request(call, name, data) as i64
}
