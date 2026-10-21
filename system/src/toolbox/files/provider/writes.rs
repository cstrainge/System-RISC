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
    Cleanup
}
pub(in crate::toolbox::files) struct State
{
    provider:  u32,
    phase:     Phase,
    orphaned:  bool,
    confirmed: Option<CatalogInfo>
}
impl State
{
    pub(in crate::toolbox::files) fn staged(&self) -> bool
    {
        self.phase == Phase::Staged
    }

    pub(in crate::toolbox::files) fn orphaned(&self) -> bool
    {
        self.orphaned
    }
}
pub(in crate::toolbox::files) struct Plan
{
    pub provider:  u32,
    pub volume:    u32,
    pub handle:    u32,
    pub parent:    u32,
    pub expected:  u32,
    pub file_type: [u8; 4],
    pub creator:   [u8; 4],
    pub when:      u64,
    pub name:      Vec<u8>,
    pub data:      Vec<u8>
}
impl Manager
{
    pub(in crate::toolbox::files) fn reserve_provider_write(&mut self, owner: u64,
                                                            call: &mut DataWriteCall, name: &str,
                                                            provider: u32)
                                                            -> Result<(), Status>
    {
        fs::FileProperties::fresh(name, call.when).validate().map_err(|_| BAD_ARGUMENT)?;
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
        call.handle = self.writes
                          .insert(Pending { owner,
                                            volume: call.volume,
                                            parent,
                                            expected_cnid: call.expected_cnid,
                                            file_type: call.file_type,
                                            creator: call.creator,
                                            when: call.when,
                                            name: input,
                                            comparison: Vec::new(),
                                            length: call.length,
                                            data,
                                            provider: Some(State { provider,
                                                                   phase: Phase::Staged,
                                                                   orphaned: false,
                                                                   confirmed: None }) })
                          .map_err(|_| NO_MEMORY)?;
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
        let mark = self.writes.get_mut(handle).unwrap();
        let state = mark.provider.as_mut().unwrap();
        state.phase = Phase::Running;
        Ok(Plan { provider: state.provider,
                  volume: mark.volume,
                  handle,
                  parent: mark.parent,
                  expected: mark.expected_cnid,
                  file_type: mark.file_type,
                  creator: mark.creator,
                  when: mark.when,
                  name: core::mem::take(&mut mark.name),
                  data: core::mem::take(&mut mark.data) })
    }

    pub(in crate::toolbox::files) fn confirm_provider_write(&mut self, handle: u32, bytes: &[u8])
                                                            -> Result<(), Status>
    {
        let mark = self.writes.get_mut(handle).ok_or(NOT_FOUND)?;
        let state = mark.provider.as_mut().ok_or(BAD_ARGUMENT)?;
        if state.phase != Phase::Running
        {
            return Err(SERVICE_BUSY);
        }
        let entry = catalog(mark.volume,
                            bytes,
                            (mark.expected_cnid != 0).then_some(mark.expected_cnid),
                            Some(mark.parent))?;
        if entry.is_directory()
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
            let mut original = state.confirmed.ok_or(BAD_ABI)?;
            original.data_length = mark.length as u64;
            original.modified = mark.when;
            if original != entry
            {
                return Err(BAD_ABI);
            }
        }
        state.confirmed = Some(entry);
        Ok(())
    }

    pub(in crate::toolbox::files) fn select_provider_target(&mut self, handle: u32, bytes: &[u8])
                                                            -> Result<(), Status>
    {
        let mark = self.writes.get_mut(handle).ok_or(NOT_FOUND)?;
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
        let state = mark.provider.as_mut().ok_or(BAD_ARGUMENT)?;
        if state.phase != Phase::Running
        {
            return Err(SERVICE_BUSY);
        }
        state.confirmed = Some(entry);
        Ok(())
    }

    pub(in crate::toolbox::files) fn publish_provider_write(&mut self, handle: u32,
                                                            receipt: fs::WriteReceipt<'_>)
                                                            -> Result<CatalogInfo, Status>
    {
        let mark = self.writes.get(handle).ok_or(NOT_FOUND)?;
        let state = mark.provider.as_ref().ok_or(BAD_ARGUMENT)?;
        if state.phase != Phase::Running
        {
            return Err(SERVICE_BUSY);
        }
        let entry = catalog_metadata(mark.volume, receipt.file, None, Some(mark.parent))?;
        if state.confirmed != Some(entry)
        {
            return Err(BAD_ABI);
        }
        let volume = mark.volume;
        let info = volume_info(volume, receipt.volume);
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
        self.writes.remove(handle).ok_or(NOT_FOUND)?;
        Ok(entry)
    }

    pub(in crate::toolbox::files) fn retire_provider_write(&mut self, handle: u32)
                                                           -> Result<(), Status>
    {
        let mark = self.writes.get(handle).ok_or(NOT_FOUND)?;
        if mark.provider.is_none()
        {
            return Err(BAD_ARGUMENT);
        }
        self.writes.remove(handle).ok_or(NOT_FOUND)?;
        Ok(())
    }

    pub(in crate::toolbox::files) fn quarantine_provider_write(&mut self, handle: u32)
                                                               -> Result<(), Status>
    {
        let state =
            self.writes.get_mut(handle).ok_or(NOT_FOUND)?.provider.as_mut().ok_or(BAD_ARGUMENT)?;
        state.phase = Phase::Quarantined;
        Ok(())
    }

    /// Staged RAM has no remote lease. An active/uncertain transaction survives
    /// its application's exit and receives one owned cleanup attempt.
    pub(in crate::toolbox::files) fn abandon_write(&mut self, handle: u32) -> bool
    {
        let mark = self.writes.get_mut(handle).unwrap();
        let Some(state) = &mut mark.provider
        else
        {
            return false;
        };
        if state.staged()
        {
            return false;
        }
        state.orphaned = true;
        if state.phase == Phase::Quarantined
        {
            state.phase = Phase::Cleanup;
        }
        #[cfg(target_arch = "riscv64")]
        super::runtime::request_write_cleanup();
        true
    }

    pub(in crate::toolbox::files) fn cleanup_provider_write(&mut self, owner: u64, handle: u32)
                                                            -> Result<(u32, u32), Status>
    {
        self.write_provider(owner, handle)?.ok_or(BAD_ARGUMENT)?;
        let mark = self.writes.get_mut(handle).unwrap();
        let state = mark.provider.as_mut().unwrap();
        if state.phase != Phase::Quarantined
        {
            return Err(SERVICE_BUSY);
        }
        state.phase = Phase::Running;
        Ok((state.provider, mark.volume))
    }

    pub(in crate::toolbox::files) fn next_orphaned_write(&mut self) -> Option<(u32, u32, u32)>
    {
        let (handle, mark) = self.writes.iter().find(|(_, mark)| {
                                                    mark.provider.as_ref().is_some_and(|state| {
                                                                              state.orphaned
                                                                              && state.phase
                                                                                 == Phase::Cleanup
                                                                          })
                                                })?;
        let result = (handle, mark.provider.as_ref()?.provider, mark.volume);
        self.writes.get_mut(handle)?.provider.as_mut()?.phase = Phase::Running;
        Some(result)
    }
}
