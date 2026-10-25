//! Owned bounded data-file transactions. Payload is staged fallibly, then the
//! block filesystem or an owned provider job publishes it. Existing resource
//! forks are preserved; no direct resource fork writer is exposed.
#![cfg_attr(not(any(target_arch = "riscv64", feature = "test-support")), allow(dead_code))]
use super::*;
pub(crate) mod lifetime;
impl super::objects::Payload for Pending
{
    const KIND: crate::toolbox::handles::KindCode = crate::toolbox::handles::KindCode(*b"FDWR");
    const WIRE: Kind = Kind::FileWrites;

    fn cleanup_pending(&self) -> bool
    {
        self.provider.as_ref().is_some_and(provider::writes::State::needs_cleanup)
    }

    fn begin_cleanup(&self)
    {
        if let Some(state) = &self.provider
        {
            state.abandon();
        }
    }

    fn scope(&self) -> Option<&super::subject::Lease>
    {
        self.authority.scope()
    }

    fn access_allowed(&self, access: super::subject::Access<'_>) -> bool
    {
        self.authority.matches(self.owner, access)
    }

    fn owner_stopping(&self) -> bool
    {
        !self.authority.accepting()
    }
}
impl lifetime::WriteHost for Manager
{
    fn release_file_write(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                          storage: bool)
                          -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        self.writes.release(ticket, storage)
    }
}
pub(super) struct Pending
{
    pub authority:     super::subject::Authority,
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
pub(super) fn status(error: MutationError) -> Status
{
    match error
    {
        MutationError::Invalid => BAD_ARGUMENT,
        MutationError::ReadOnly => READ_ONLY,
        MutationError::NotFound => NOT_FOUND,
        MutationError::NotEmpty => DIRECTORY_NOT_EMPTY,
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
        if call.operation == BEGIN_DATA_WRITE
        {
            #[cfg(test)]
            let authority = self.fixture_write_owner(owner);
            #[cfg(all(not(test), target_arch = "riscv64"))]
            let authority = if owner == 0
            {
                crate::toolbox::handles::system_owner()?
            }
            else
            {
                return Err(ACCESS_DENIED);
            };
            #[cfg(all(not(test), not(target_arch = "riscv64")))]
            return Err(ACCESS_DENIED);
            #[cfg(any(test, target_arch = "riscv64"))]
            return self.write_request_for(owner, &authority, call, name, data);
        }
        self.write_request_inner(owner, None, call, name, data)
    }

    pub(crate) fn write_request_for(&mut self, owner: u64,
                                    authority: &crate::toolbox::handles::OwnerRef,
                                    call: &mut DataWriteCall, name: &str, data: &[u8])
                                    -> Result<(), Status>
    {
        self.validate_file_authority(owner, authority)?;
        #[cfg(target_arch = "riscv64")]
        self.prepare_writes(crate::toolbox::handles::file_write_kinds()?)?;
        self.write_request_inner(owner, Some(authority), call, name, data)
    }

    fn write_request_inner(&mut self, owner: u64,
                           authority: Option<&crate::toolbox::handles::OwnerRef>,
                           call: &mut DataWriteCall, name: &str, data: &[u8])
                           -> Result<(), Status>
    {
        if call.reserved != [0; 2]
        {
            return Err(BAD_ARGUMENT);
        }
        if call.operation == BEGIN_DATA_WRITE
        {
            let authority = authority.ok_or(ACCESS_DENIED)?;
            if call.length > MAX_DATA_WRITE
            {
                return Err(NO_MEMORY);
            }
            self.validate_transfer_mutation(owner, call.volume)?;
            if call.expected_cnid != 0 && self.file_pinned(call.volume, call.expected_cnid)
            {
                return Err(SERVICE_BUSY);
            }
            self.validate_provider_mutation(call.volume)?;
            if let Some(provider) = self.read_provider(call.volume)?
            {
                return self.reserve_provider_write_for(owner, authority, call, name, provider);
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
            call.handle =
                self.writes
                    .insert(authority,
                            Pending { authority:
                                          super::subject::Authority::Client(authority.clone()),
                                      owner,
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
                                      directory: false })?;
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
                let mut protected = Vec::new();
                let capacity =
                    self.handles.len().checked_add(self.streams.len()).ok_or(NO_MEMORY)?;
                protected.try_reserve_exact(capacity).map_err(|_| NO_MEMORY)?;
                let pending = self.writes.get(call.handle).ok_or(NOT_FOUND)?;
                let (volume_id, parent) = (pending.volume, pending.parent);
                protected.extend(self.handles
                                     .iter()
                                     .filter(|(_, file)| file.volume == volume_id)
                                     .map(|(_, file)| file.id));
                protected.extend(self.streams
                                     .iter()
                                     .filter(|(_, mark)| mark.volume == volume_id)
                                     .map(|(_, mark)| mark.expected)
                                     .filter(|id| *id != 0));
                // Reserve the catalog/watch revision before any side effect.
                // A refused commit leaves the same staged bytes and handle.
                let revision = scan::next_revision()?;
                let Mounted::Block(mounted) = self.volumes.get_mut(volume_id).ok_or(NOT_FOUND)?
                else
                {
                    return Err(NOT_SUPPORTED);
                };
                if mounted.poisoned
                {
                    return Err(ROLLBACK_FAILED);
                }
                let mut volume = io(Volume::mount(&mut mounted.device, mounted.partition))?;
                let result =
                    volume.save_data(mac_filesystem::DataSave { parent,
                                                                name: &pending.name,
                                                                expected: pending.expected_cnid,
                                                                file_type: pending.file_type,
                                                                creator: pending.creator,
                                                                when: pending.when },
                                     &pending.data,
                                     &protected);
                mounted.scanner = volume.catalog_scanner().ok();
                match result
                {
                    Ok(entry) =>
                    {
                        mounted.info.free_bytes = volume.free_bytes;
                        mounted.info.modified = volume.modified;
                        mounted.catalog_revision = revision;
                        call.entry = metadata(volume_id, &entry);
                        self.writes.remove(call.handle).ok_or(NOT_FOUND)?;
                        self.notify_directory_at(volume_id, parent, revision);
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
pub(super) unsafe fn dispatch(manager: &mut Manager, owner: u64,
                              authority: Option<&crate::toolbox::handles::OwnerRef>,
                              pointer: *mut (), size: usize)
                              -> i64
{
    unsafe {
        checked(pointer, size, |call, name, data| {
            if call.operation == BEGIN_DATA_WRITE
            {
                match authority
                {
                    Some(authority) =>
                    {
                        manager.write_request_for(owner, authority, call, name, data)
                               .err()
                               .unwrap_or(OK)
                    }
                    None => ACCESS_DENIED
                }
            }
            else
            {
                manager.write_request(owner, call, name, data).err().unwrap_or(OK)
            }
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

#[cfg(test)]
impl Manager
{
    pub(super) fn fixture_write_owner(&mut self, label: u64) -> crate::toolbox::handles::OwnerRef
    {
        use mac_object_table::domain::{DomainKind, OwnerIndex};

        use crate::toolbox::handles::{KindCode, OwnerRef};
        let owner = self.handles.fixture_owner(label);
        let registry = &mut self.handles.fixture.as_mut().unwrap().0;
        let kinds = match (registry.get(KindCode(*b"FDWR")), registry.get(KindCode(*b"FDST")))
        {
            (Ok(payload), Ok(storage)) => (payload, storage),
            _ =>
            {
                let registrar =
                    OwnerRef::try_new(OwnerIndex::issue(DomainKind::Module).unwrap()).unwrap();
                (registry.register(&registrar,
                                   lifetime::specification(false),
                                   lifetime::ReleaseWrites)
                         .unwrap(),
                 registry.register(&registrar,
                                   lifetime::specification(true),
                                   lifetime::ReleaseStorage)
                         .unwrap())
            }
        };
        self.prepare_writes(kinds).unwrap();
        owner
    }
}
