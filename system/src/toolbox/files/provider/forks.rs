//! Resident fork marks. Provider tokens use the same generation-tagged table
//! as block-backed marks and remain reserved through acknowledged cleanup.
#![forbid(unsafe_code)]
use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase
{
    Opening,
    Ready,
    Reading,
    Closing,
    Quarantined
}
pub(in crate::toolbox::files) struct Open
{
    kind:                                   u32,
    length:                                 u64,
    phase:                                  Phase,
    pub(in crate::toolbox::files) orphaned: bool
}
impl Open
{
    pub(in crate::toolbox::files) fn ready_length(&self) -> Result<u64, Status>
    {
        match self.phase
        {
            Phase::Ready if !self.orphaned => Ok(self.length),
            Phase::Quarantined => Err(ROLLBACK_FAILED),
            _ => Err(SERVICE_BUSY)
        }
    }

    pub(in crate::toolbox::files) fn abandon(&mut self)
    {
        self.orphaned = true;
        if self.phase == Phase::Quarantined
        {
            // One final cleanup attempt on app exit, not a per-tick retry loop.
            self.phase = Phase::Ready;
        }
        signal_cleanup();
    }
}
fn signal_cleanup()
{
    #[cfg(target_arch = "riscv64")]
    super::runtime::request_fork_cleanup();
}
impl Manager
{
    pub(in crate::toolbox::files) fn file_provider(&self, owner: u64, id: u32)
                                                   -> Result<Option<u32>, Status>
    {
        let file = self.handles.get(id).ok_or(NOT_FOUND)?;
        if file.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        match &file.fork
        {
            FileFork::Block(_) => Ok(None),
            FileFork::Provider(_) =>
            {
                match self.volumes.get(file.volume).ok_or(NOT_FOUND)?
                {
                    super::super::Mounted::Provider(mounted) => Ok(Some(mounted.provider)),
                    _ => Err(BAD_ABI)
                }
            }
        }
    }

    fn provider_file(&mut self, id: u32) -> Result<(&mut OpenFile, u32), Status>
    {
        let file = self.handles.get_mut(id).ok_or(NOT_FOUND)?;
        if !matches!(file.fork, FileFork::Provider(_))
        {
            return Err(BAD_ARGUMENT);
        }
        let provider = match self.volumes.get(file.volume).ok_or(NOT_FOUND)?
        {
            super::super::Mounted::Provider(mounted) => mounted.provider,
            _ => return Err(BAD_ABI)
        };
        Ok((file, provider))
    }

    pub(in crate::toolbox::files) fn reserve_provider_file(&mut self, owner: u64, volume: u32,
                                                           node: u32, kind: u32)
                                                           -> Result<(u32, u32), Status>
    {
        self.validate_transfer_read(owner, volume, node)?;
        if node < ROOT_ID || !matches!(kind, DATA_FORK | RESOURCE_FORK)
        {
            return Err(BAD_ARGUMENT);
        }
        let provider = self.read_provider(volume)?.ok_or(BAD_ARGUMENT)?;
        if self.streams.iter().any(|(_, mark)| mark.volume == volume && mark.expected == node)
        {
            return Err(SERVICE_BUSY);
        }
        if self.writes.iter().any(|(_, mark)| {
                                 mark.volume == volume
                                 && mark.expected_cnid == node
                                 && mark.provider.as_ref().is_some_and(|state| !state.staged())
                             })
        {
            return Err(SERVICE_BUSY);
        }
        let handle = self.handles
                         .insert(OpenFile { owner,
                                            volume,
                                            id: node,
                                            position: 0,
                                            resource_index: None,
                                            fork: FileFork::Provider(Open { kind,
                                                                            length: 0,
                                                                            phase: Phase::Opening,
                                                                            orphaned: false }) })
                         .map_err(|_| NO_MEMORY)?;
        Ok((provider, handle))
    }

    pub(in crate::toolbox::files) fn publish_provider_file(&mut self, id: u32, bytes: &[u8])
                                                           -> Result<u64, Status>
    {
        let bytes: &[u8; 16] = bytes.try_into().map_err(|_| BAD_ABI)?;
        let (file, _) = self.provider_file(id)?;
        let FileFork::Provider(fork) = &mut file.fork
        else
        {
            return Err(BAD_ABI);
        };
        if fork.phase != Phase::Opening || fork.orphaned
        {
            return Err(SERVICE_BUSY);
        }
        let at = if fork.kind == DATA_FORK { 0 } else { 8 };
        fork.length = u64::from_le_bytes(bytes[at..at + 8].try_into().map_err(|_| BAD_ABI)?);
        fork.phase = Phase::Ready;
        Ok(fork.length)
    }

    pub(in crate::toolbox::files) fn begin_provider_file_read(
        &mut self, owner: u64, id: u32, capacity: usize)
        -> Result<(u32, fs::Arguments<'static>), Status>
    {
        let (file, provider) = self.provider_file(id)?;
        if file.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        let FileFork::Provider(fork) = &mut file.fork
        else
        {
            return Err(BAD_ABI);
        };
        let length = fork.ready_length()?;
        let remaining = length.checked_sub(file.position).ok_or(BAD_ABI)?;
        let count = remaining.min(capacity.min(fs::MAX_TRANSFER) as u64);
        fork.phase = Phase::Reading;
        Ok((provider,
            fs::Arguments { volume: file.volume,
                            token: id,
                            flags: fork.kind,
                            offset: file.position,
                            length: count,
                            ..Default::default() }))
    }

    pub(in crate::toolbox::files) fn finish_provider_file_read(&mut self, id: u32, offset: u64,
                                                               count: u64,
                                                               delivered: Option<usize>)
                                                               -> Result<(u64, u64), Status>
    {
        let (file, _) = self.provider_file(id)?;
        let FileFork::Provider(fork) = &mut file.fork
        else
        {
            return Err(BAD_ABI);
        };
        if fork.phase != Phase::Reading || file.position != offset
        {
            return Err(BAD_ABI);
        }
        // Restore tracking even on provider errors or abandoned callers.
        fork.phase = Phase::Ready;
        if fork.orphaned
        {
            signal_cleanup();
        }
        if let Some(delivered) = delivered
        {
            if delivered as u64 > count || count > fork.length.saturating_sub(offset)
            {
                return Err(BAD_ABI);
            }
            file.position = offset.checked_add(delivered as u64).ok_or(BAD_ABI)?;
        }
        Ok((file.position, fork.length))
    }

    pub(in crate::toolbox::files) fn begin_provider_file_close(&mut self, owner: u64, id: u32)
                                                               -> Result<(u32, u32), Status>
    {
        let (file, provider) = self.provider_file(id)?;
        if file.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        let FileFork::Provider(fork) = &mut file.fork
        else
        {
            return Err(BAD_ABI);
        };
        if !matches!(fork.phase, Phase::Ready | Phase::Quarantined)
        {
            return Err(SERVICE_BUSY);
        }
        fork.phase = Phase::Closing;
        Ok((provider, file.volume))
    }

    pub(in crate::toolbox::files) fn cleanup_provider_file(&mut self, id: u32)
                                                           -> Result<u32, Status>
    {
        let (file, _) = self.provider_file(id)?;
        let FileFork::Provider(fork) = &mut file.fork
        else
        {
            return Err(BAD_ABI);
        };
        if !matches!(fork.phase, Phase::Opening | Phase::Ready)
        {
            return Err(SERVICE_BUSY);
        }
        fork.orphaned = true;
        fork.phase = Phase::Closing;
        Ok(file.volume)
    }

    pub(in crate::toolbox::files) fn quarantine_provider_file(&mut self, id: u32)
                                                              -> Result<(), Status>
    {
        let (file, _) = self.provider_file(id)?;
        let FileFork::Provider(fork) = &mut file.fork
        else
        {
            return Err(BAD_ABI);
        };
        fork.phase = Phase::Quarantined;
        Ok(())
    }

    pub(in crate::toolbox::files) fn retire_provider_file(&mut self, id: u32)
                                                          -> Result<(), Status>
    {
        let (file, _) = self.provider_file(id)?;
        let FileFork::Provider(fork) = &file.fork
        else
        {
            return Err(BAD_ABI);
        };
        if !matches!(fork.phase, Phase::Opening | Phase::Closing)
        {
            return Err(SERVICE_BUSY);
        }
        self.handles.remove(id).ok_or(NOT_FOUND)?;
        Ok(())
    }

    pub(in crate::toolbox::files) fn next_orphaned_file(
        &mut self)
        -> Result<Option<(u32, u32, u32)>, Status>
    {
        let id = self.handles.iter().find_map(|(id, file)| matches!(&file.fork, FileFork::Provider(fork) if fork.orphaned && fork.phase == Phase::Ready).then_some(id));
        let Some(id) = id
        else
        {
            return Ok(None);
        };
        let (file, provider) = self.provider_file(id)?;
        let FileFork::Provider(fork) = &mut file.fork
        else
        {
            return Err(BAD_ABI);
        };
        fork.phase = Phase::Closing;
        Ok(Some((id, provider, file.volume)))
    }
}
