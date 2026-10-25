//! Generation-checked resident write marks. Publication and cleanup retain the
//! mark (and therefore its mounted volume) until the provider acknowledges it.
#![forbid(unsafe_code)]
use super::{super::write::Pending, *};
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase
{
    Staged,
    Running,
    Quarantined,
    Cleanup,
    Closed
}
pub(in crate::toolbox::files) struct State
{
    provider: u32,
    progress: spinning_top::Spinlock<Progress>
}
#[derive(Clone, Copy)]
struct Progress
{
    phase:     Phase,
    orphaned:  bool,
    confirmed: Option<CatalogInfo>,
    metadata:  Option<u32>
}
impl State
{
    fn new(provider: u32, phase: Phase, metadata: Option<u32>) -> Self
    {
        Self { provider,
               progress: spinning_top::Spinlock::new(Progress { phase,
                                                                orphaned: false,
                                                                confirmed: None,
                                                                metadata }) }
    }

    pub(in crate::toolbox::files) fn staged(&self) -> bool
    {
        self.progress.lock().phase == Phase::Staged
    }

    pub(in crate::toolbox::files) fn needs_cleanup(&self) -> bool
    {
        !matches!(self.progress.lock().phase, Phase::Staged | Phase::Closed)
    }

    pub(in crate::toolbox::files) fn abandon(&self)
    {
        let mut progress = self.progress.lock();
        progress.orphaned = true;
        if progress.phase == Phase::Quarantined
        {
            progress.phase = Phase::Cleanup;
        }
        drop(progress);
        #[cfg(target_arch = "riscv64")]
        super::runtime::request_write_cleanup();
    }
}
pub(in crate::toolbox::files) struct Plan
{
    pub provider:    u32,
    pub volume:      u32,
    pub handle:      u32,
    pub parent:      u32,
    pub expected:    u32,
    pub file_type:   [u8; 4],
    pub creator:     [u8; 4],
    pub when:        u64,
    pub name:        Vec<u8>,
    pub data:        Vec<u8>,
    pub directory:   bool,
    pub finder_info: [u8; 32]
}
impl Manager
{
    pub(in crate::toolbox::files) fn reserve_provider_metadata_for(&mut self, owner: u64,
                                                                   authority:&crate::toolbox::handles::OwnerRef,
                                                                   call: &FileCall)
                                                                   -> Result<(u32, u32), Status>
    {
        self.validate_file_authority(owner, authority)?;
        #[cfg(target_arch = "riscv64")]
        self.prepare_writes(crate::toolbox::handles::file_write_kinds()?)?;
        self.validate_transfer_mutation(owner, call.volume)?;
        if call.operation == REMOVE && self.file_pinned(call.volume, call.cnid)
        {
            return Err(SERVICE_BUSY);
        }
        self.validate_provider_mutation(call.volume)?;
        if self.streams
               .iter()
               .any(|(_, mark)| mark.volume == call.volume && mark.expected == call.cnid)
        {
            return Err(SERVICE_BUSY);
        }
        let provider = self.read_provider(call.volume)?.ok_or(BAD_ARGUMENT)?;
        if call.cnid < ROOT_ID
        {
            return Err(BAD_ARGUMENT);
        }
        if !matches!(call.operation, SET_ICON_LOCATION | SET_FOLDER_STATE)
           && self.handles
                  .iter()
                  .any(|(_, file)| file.volume == call.volume && file.id == call.cnid)
        {
            return Err(ACCESS_DENIED);
        }
        if self.writes
               .iter()
               .any(|(_, mark)| mark.volume == call.volume && mark.expected_cnid == call.cnid)
        {
            return Err(SERVICE_BUSY);
        }
        let handle = self.writes.insert(authority,
                                         Pending { authority: authority.clone(),
                                                   owner,
                                                   volume: call.volume,
                                                   parent: if call.operation == MOVE_INTO
                                                   {
                                                       call.index
                                                   }
                                                   else
                                                   {
                                                       0
                                                   },
                                                   expected_cnid: call.cnid,
                                                   file_type: [0; 4],
                                                   creator: [0; 4],
                                                   when: call.position,
                                                   name: Vec::new(),
                                                   comparison: Vec::new(),
                                                   length: 0,
                                                   data: Vec::new(),
                                                   directory: false,
                                                   provider:
                                                       Some(State::new(provider,
                                                                       Phase::Running,
                                                                       Some(call.operation))) })?;
        Ok((provider, handle))
    }

    pub(in crate::toolbox::files) fn confirm_provider_metadata(&mut self, handle: u32,
                                                               bytes: &[u8], name: &str)
                                                               -> Result<CatalogInfo, Status>
    {
        let mark = self.writes.completing(handle).ok_or(NOT_FOUND)?;
        let entry = catalog(mark.volume, bytes, Some(mark.expected_cnid), None)?;
        let state = mark.provider.as_ref().ok_or(BAD_ARGUMENT)?;
        let mut progress = state.progress.lock();
        let operation = progress.metadata.ok_or(BAD_ARGUMENT)?;
        if matches!(operation, SET_LOCKED | SET_STATIONERY) && entry.is_directory()
        {
            return Err(BAD_ARGUMENT);
        }
        if operation == SET_FOLDER_STATE && !entry.is_directory()
        {
            return Err(BAD_ARGUMENT);
        }
        if operation == SET_COMMENT && entry.id == ROOT_ID
        {
            return Err(ACCESS_DENIED);
        }
        if operation == MOVE_INTO && entry.parent != mark.parent
        {
            return Err(BAD_ABI);
        }
        if operation == RENAME
           && entry.name().as_bytes()
              != mac_filesystem::Format::HfsB.prepare_name(name).map_err(|_| BAD_ARGUMENT)?.0
        {
            return Err(BAD_ABI);
        }
        progress.confirmed = Some(entry);
        Ok(entry)
    }

    pub(in crate::toolbox::files) fn reserve_provider_directory_for(&mut self, owner: u64,
                                                                    authority:&crate::toolbox::handles::OwnerRef,
                                                                    volume: u32, parent: u32,
                                                                    name: &str, when: u64)
                                                                    -> Result<Plan, Status>
    {
        self.validate_file_authority(owner, authority)?;
        #[cfg(target_arch = "riscv64")]
        self.prepare_writes(crate::toolbox::handles::file_write_kinds()?)?;
        self.validate_transfer_mutation(owner, volume)?;
        self.validate_provider_mutation(volume)?;
        let provider = self.read_provider(volume)?.ok_or(BAD_ARGUMENT)?;
        let mut call = DataWriteCall { volume,
                                       parent,
                                       when,
                                       ..Default::default() };
        self.reserve_provider_write_for(owner, authority, &mut call, name, provider)?;
        self.writes.get_mut(call.handle).ok_or(NOT_FOUND)?.directory = true;
        match self.prepare_provider_write(owner, call.handle)
        {
            Ok(plan) => Ok(plan),
            Err(error) =>
            {
                self.writes.remove(call.handle);
                Err(error)
            }
        }
    }

    pub(in crate::toolbox::files) fn reserve_provider_write_for(&mut self, owner: u64,
                                                                authority:&crate::toolbox::handles::OwnerRef,
                                                                call: &mut DataWriteCall,
                                                                name: &str, provider: u32)
                                                                -> Result<(), Status>
    {
        self.validate_file_authority(owner, authority)?;
        #[cfg(target_arch = "riscv64")]
        self.prepare_writes(crate::toolbox::handles::file_write_kinds()?)?;
        self.validate_transfer_mutation(owner, call.volume)?;
        if call.expected_cnid != 0 && self.file_pinned(call.volume, call.expected_cnid)
        {
            return Err(SERVICE_BUSY);
        }
        fs::FileProperties::fresh(name, call.when).validate().map_err(|_| BAD_ARGUMENT)?;
        let (file_type, creator) =
            super::super::types::classify(name, call.file_type, call.creator);
        let parent = if call.parent == 0 { ROOT_ID } else { call.parent };
        if parent < ROOT_ID || call.expected_cnid == 1
        {
            return Err(BAD_ARGUMENT);
        }
        self.writes.reserve().map_err(|_| NO_MEMORY)?;
        let mut input = Vec::new();
        input.try_reserve_exact(name.len()).map_err(|_| NO_MEMORY)?;
        input.extend_from_slice(name.as_bytes());
        let mut data = Vec::new();
        data.try_reserve_exact(call.length).map_err(|_| NO_MEMORY)?;
        call.handle = self.writes.insert(authority,
                                          Pending { authority: authority.clone(),
                                                    owner,
                                                    volume: call.volume,
                                                    parent,
                                                    expected_cnid: call.expected_cnid,
                                                    file_type,
                                                    creator,
                                                    when: call.when,
                                                    name: input,
                                                    comparison: Vec::new(),
                                                    length: call.length,
                                                    data,
                                                    directory: false,
                                                    provider: Some(State::new(provider,
                                                                              Phase::Staged,
                                                                              None)) })?;
        Ok(())
    }

    pub(in crate::toolbox::files) fn write_provider(&self, owner: u64, handle: u32)
                                                    -> Result<Option<u32>, Status>
    {
        let mark = self.writes.get(handle).ok_or(NOT_FOUND)?;
        if mark.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        Ok(mark.provider.as_ref().map(|state| state.provider))
    }

    pub(in crate::toolbox::files) fn prepare_provider_write(&mut self, owner: u64, handle: u32)
                                                            -> Result<Plan, Status>
    {
        self.write_provider(owner, handle)?.ok_or(BAD_ARGUMENT)?;
        let mark = self.writes.get(handle).ok_or(NOT_FOUND)?;
        if !mark.provider.as_ref().is_some_and(State::staged)
        {
            return Err(SERVICE_BUSY);
        }
        if mark.data.len() != mark.length
        {
            return Err(BAD_ARGUMENT);
        }
        self.validate_provider_mutation(mark.volume)?;
        if mark.expected_cnid != 0
           && self.handles
                  .iter()
                  .any(|(_, file)| file.volume == mark.volume && file.id == mark.expected_cnid)
        {
            return Err(SERVICE_BUSY);
        }
        let mark = self.writes.get_mut(handle).ok_or(NOT_FOUND)?;
        let state = mark.provider.as_ref().ok_or(BAD_ARGUMENT)?;
        state.progress.lock().phase = Phase::Running;
        Ok(Plan { provider: state.provider,
                  volume: mark.volume,
                  handle,
                  parent: mark.parent,
                  expected: mark.expected_cnid,
                  file_type: mark.file_type,
                  creator: mark.creator,
                  when: mark.when,
                  name: core::mem::take(&mut mark.name),
                  data: core::mem::take(&mut mark.data),
                  directory: mark.directory,
                  finder_info: [0; 32] })
    }

    pub(in crate::toolbox::files) fn confirm_provider_write(&mut self, handle: u32, bytes: &[u8])
                                                            -> Result<(), Status>
    {
        let mark = self.writes.completing(handle).ok_or(NOT_FOUND)?;
        let state = mark.provider.as_ref().ok_or(BAD_ARGUMENT)?;
        let mut progress = state.progress.lock();
        if progress.phase != Phase::Running
        {
            return Err(SERVICE_BUSY);
        }
        let entry = catalog(mark.volume,
                            bytes,
                            (mark.expected_cnid != 0).then_some(mark.expected_cnid),
                            Some(mark.parent))?;
        if entry.is_directory() != mark.directory
           || entry.data_length != mark.length as u64
           || mark.expected_cnid == 0
              && (entry.resource_length != 0
                  || entry.file_type != mark.file_type
                  || entry.creator != mark.creator
                  || entry.created != mark.when
                  || entry.modified != mark.when)
        {
            return Err(BAD_ABI);
        }
        if mark.expected_cnid != 0
        {
            let mut original = progress.confirmed.ok_or(BAD_ABI)?;
            original.data_length = mark.length as u64;
            original.modified = mark.when;
            if original != entry
            {
                return Err(BAD_ABI);
            }
        }
        progress.confirmed = Some(entry);
        Ok(())
    }

    pub(in crate::toolbox::files) fn select_provider_target(&mut self, handle: u32, bytes: &[u8])
                                                            -> Result<(), Status>
    {
        let mark = self.writes.completing(handle).ok_or(NOT_FOUND)?;
        let entry = catalog(mark.volume, bytes, None, Some(mark.parent))?;
        if mark.expected_cnid == 0
        {
            return Err(ALREADY_EXISTS);
        }
        if entry.id != mark.expected_cnid
        {
            return Err(NOT_FOUND);
        }
        if entry.is_directory()
        {
            return Err(BAD_ARGUMENT);
        }
        if entry.flags & 1 != 0
        {
            return Err(ACCESS_DENIED);
        }
        let state = mark.provider.as_ref().ok_or(BAD_ARGUMENT)?;
        let mut progress = state.progress.lock();
        if progress.phase != Phase::Running
        {
            return Err(SERVICE_BUSY);
        }
        progress.confirmed = Some(entry);
        Ok(())
    }

    pub(in crate::toolbox::files) fn publish_provider_write(&mut self, handle: u32,
                                                            receipt: fs::WriteReceipt<'_>)
                                                            -> Result<CatalogInfo, Status>
    {
        let mark = self.writes.completing(handle).ok_or(NOT_FOUND)?;
        let state = mark.provider.as_ref().ok_or(BAD_ARGUMENT)?;
        let progress = state.progress.lock();
        if progress.phase != Phase::Running
        {
            return Err(SERVICE_BUSY);
        }
        let parent = if progress.metadata.is_some()
        {
            progress.confirmed.ok_or(BAD_ABI)?.parent
        }
        else
        {
            mark.parent
        };
        let entry = catalog_metadata(mark.volume, receipt.file, None, Some(parent))?;
        let expected = progress.confirmed.ok_or(BAD_ABI)?;
        if expected != entry
        {
            return Err(BAD_ABI);
        }
        let volume = mark.volume;
        let info = volume_info(volume, receipt.volume);
        drop(progress);
        let mounted = self.provider_mount(volume)?;
        let prior = mounted.info.ok_or(BAD_ABI)?;
        if info.filesystem != prior.filesystem
           || info.filesystem_version != prior.filesystem_version
        {
            return Err(BAD_ABI);
        }
        // Fresh per acknowledged change, including out-of-order completions.
        let revision = super::super::scan::next_revision()?;
        mounted.info = Some(info);
        mounted.revision = revision;
        self.finish_write_mark(handle)?;
        Ok(entry)
    }

    pub(in crate::toolbox::files) fn write_is_removal(&self, handle: u32) -> Result<bool, Status>
    {
        let mark = self.writes.completing(handle).ok_or(NOT_FOUND)?;
        Ok(mark.provider.as_ref().ok_or(BAD_ARGUMENT)?.progress.lock().metadata == Some(REMOVE))
    }

    pub(in crate::toolbox::files) fn publish_provider_removal(
        &mut self, handle: u32, receipt: fs::RemovalReceipt<'_>)
        -> Result<Option<CatalogInfo>, Status>
    {
        let mark = self.writes.completing(handle).ok_or(NOT_FOUND)?;
        let state = mark.provider.as_ref().ok_or(BAD_ARGUMENT)?;
        let progress = state.progress.lock();
        if progress.metadata != Some(REMOVE) || progress.phase != Phase::Running
        {
            return Err(BAD_ARGUMENT);
        }
        let volume = mark.volume;
        let entry = progress.confirmed;
        if !receipt.removed
        {
            return Ok(None);
        }
        let entry = entry.ok_or(BAD_ABI)?;
        let info = volume_info(volume, receipt.volume);
        let revision = super::super::scan::next_revision()?;
        drop(progress);
        let mounted = self.provider_mount(volume)?;
        let prior = mounted.info.ok_or(BAD_ABI)?;
        if info.filesystem != prior.filesystem
           || info.filesystem_version != prior.filesystem_version
        {
            return Err(BAD_ABI);
        }
        mounted.info = Some(info);
        mounted.revision = revision;
        mounted.changes = true;
        self.notify_directory(volume, entry.parent)?;
        self.finish_write_mark(handle)?;
        Ok(Some(entry))
    }

    pub(in crate::toolbox::files) fn retire_provider_write(&mut self, handle: u32)
                                                           -> Result<(), Status>
    {
        let mark = self.writes.completing(handle).ok_or(NOT_FOUND)?;
        if mark.provider.is_none()
        {
            return Err(BAD_ARGUMENT);
        }
        self.finish_write_mark(handle)?;
        Ok(())
    }

    pub(in crate::toolbox::files) fn quarantine_provider_write(&mut self, handle: u32)
                                                               -> Result<(), Status>
    {
        let state = self.writes
                        .completing(handle)
                        .ok_or(NOT_FOUND)?
                        .provider
                        .as_ref()
                        .ok_or(BAD_ARGUMENT)?;
        state.progress.lock().phase = Phase::Quarantined;
        Ok(())
    }

    pub(in crate::toolbox::files) fn cleanup_provider_write(&mut self, owner: u64, handle: u32)
                                                            -> Result<(u32, u32), Status>
    {
        self.write_provider(owner, handle)?.ok_or(BAD_ARGUMENT)?;
        let mark = self.writes.get_mut(handle).ok_or(NOT_FOUND)?;
        let state = mark.provider.as_ref().ok_or(BAD_ARGUMENT)?;
        let mut progress = state.progress.lock();
        if progress.phase != Phase::Quarantined
        {
            return Err(SERVICE_BUSY);
        }
        progress.phase = Phase::Running;
        Ok((state.provider, mark.volume))
    }

    pub(in crate::toolbox::files) fn next_orphaned_write(&mut self) -> Option<(u32, u32, u32)>
    {
        let (handle, mark) =
            self.writes.iter().find(|(_, mark)| {
                                   mark.provider.as_ref().is_some_and(|state| {
                                                             let progress = state.progress.lock();
                                                             progress.orphaned
                                                             && progress.phase == Phase::Cleanup
                                                         })
                               })?;
        let state = mark.provider.as_ref()?;
        state.progress.lock().phase = Phase::Running;
        Some((handle, state.provider, mark.volume))
    }

    fn finish_write_mark(&mut self, handle: u32) -> Result<(), Status>
    {
        let mark = self.writes.completing(handle).ok_or(NOT_FOUND)?;
        let state = mark.provider.as_ref().ok_or(BAD_ARGUMENT)?;
        state.progress.lock().phase = Phase::Closed;
        self.writes.remove(handle).ok_or(NOT_FOUND)?;
        Ok(())
    }
}
