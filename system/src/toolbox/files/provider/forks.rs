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
    Quarantined,
    Closed
}
pub(in crate::toolbox::files) struct Open
{
    kind:  u32,
    state: spinning_top::Spinlock<State>
}
struct State
{
    length:   u64,
    phase:    Phase,
    orphaned: bool
}
impl Open
{
    pub(in crate::toolbox::files) fn ready_length(&self) -> Result<u64, Status>
    {
        let state = self.state.lock();
        match state.phase
        {
            Phase::Ready if !state.orphaned => Ok(state.length),
            Phase::Quarantined => Err(ROLLBACK_FAILED),
            _ => Err(SERVICE_BUSY)
        }
    }

    pub(in crate::toolbox::files) fn orphaned(&self) -> bool
    {
        self.state.lock().orphaned
    }

    pub(in crate::toolbox::files) fn abandon(&self)
    {
        let mut state = self.state.lock();
        state.orphaned = true;
        if state.phase == Phase::Quarantined
        {
            state.phase = Phase::Ready;
        }
        drop(state);
        signal_cleanup();
    }

    pub(in crate::toolbox::files) fn closed(&self) -> bool
    {
        self.state.lock().phase == Phase::Closed
    }

    fn cleanup_ready(&self) -> bool
    {
        let state = self.state.lock();
        state.orphaned && state.phase == Phase::Ready
    }
}
fn signal_cleanup()
{
    #[cfg(target_arch = "riscv64")]
    super::runtime::request_fork_cleanup();
}
impl Manager
{
    pub(in crate::toolbox::files) fn file_provider<'a>(&self,
                                                       owner: impl Into<super::super::subject::Access<'a>>,
                                                       id: u32)
                                                       -> Result<Option<u64>, Status>
    {
        let file = self.slot_access(owner.into(), id)?;
        match &*file.fork.lock()
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

    fn provider_file(&self, id: u32) -> Result<(&OpenFile, u64), Status>
    {
        let file = self.handles.completing(id).ok_or(NOT_FOUND)?;
        if !matches!(&*file.fork.lock(), FileFork::Provider(_))
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

    pub(in crate::toolbox::files) fn reserve_provider_file_for(&mut self, owner: u64,
                                                               authority:&crate::toolbox::handles::OwnerRef,
                                                               volume: u32, node: u32, kind: u32)
                                                               -> Result<(u64, u32), Status>
    {
        self.reserve_provider_file_authorized(super::super::subject::Access::Client(owner),&super::super::subject::Authority::Client(authority.clone()),volume,node,kind)
    }

    pub(in crate::toolbox::files) fn reserve_provider_file_authorized(
        &mut self, access: super::super::subject::Access<'_>,
        authority: &super::super::subject::Authority, volume: u32, node: u32, kind: u32)
        -> Result<(u64, u32), Status>
    {
        self.validate_creation(access, authority, crate::toolbox::handles::KindCode(*b"FFRK"))?;
        #[cfg(target_arch = "riscv64")]
        self.prepare_forks(crate::toolbox::handles::file_fork_kinds()?)?;
        self.validate_transfer_read(access, volume, node)?;
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
        #[cfg(target_arch = "riscv64")]
        let io_subject = Some(crate::drivers::jobs::subjects::resident()?.admit(authority)?);
        let handle = self.handles
                         .insert_authorized(authority, OpenFile { #[cfg(target_arch = "riscv64")] io_subject, owner:access.label(),
                                            volume,
                                            id: node,
                                            position: core::sync::atomic::AtomicU64::new(0),
                                            authority: authority.clone(),
                                            resource_index: None,
                                            fork: spinning_top::Spinlock::new(FileFork::Provider(Open { kind,
                                                                            state: spinning_top::Spinlock::new(State { length: 0, phase: Phase::Opening, orphaned: false }) })) })
                         ?;
        Ok((provider, handle))
    }

    #[cfg(any(test, target_arch = "riscv64"))]
    pub(in crate::toolbox::files) fn reserve_provider_file(&mut self, owner: u64, volume: u32,
                                                           node: u32, kind: u32)
                                                           -> Result<(u64, u32), Status>
    {
        #[cfg(test)]
        let authority = self.handles.fixture_owner(owner);
        #[cfg(all(not(test), target_arch = "riscv64"))]
        let authority = if owner == 0
        {
            crate::toolbox::handles::system_owner()?
        }
        else
        {
            return Err(ACCESS_DENIED);
        };
        self.reserve_provider_file_for(owner, &authority, volume, node, kind)
    }

    pub(in crate::toolbox::files) fn publish_provider_file(&mut self, id: u32, bytes: &[u8])
                                                           -> Result<u64, Status>
    {
        let bytes: &[u8; 16] = bytes.try_into().map_err(|_| BAD_ABI)?;
        let (file, _) = self.provider_file(id)?;
        let FileFork::Provider(fork) = &*file.fork.lock()
        else
        {
            return Err(BAD_ABI);
        };
        let mut state = fork.state.lock();
        if state.phase != Phase::Opening || state.orphaned
        {
            return Err(SERVICE_BUSY);
        }
        let at = if fork.kind == DATA_FORK { 0 } else { 8 };
        state.length = u64::from_le_bytes(bytes[at..at + 8].try_into().map_err(|_| BAD_ABI)?);
        state.phase = Phase::Ready;
        Ok(state.length)
    }

    pub(in crate::toolbox::files) fn begin_provider_file_read<'a>(
        &mut self, owner: impl Into<super::super::subject::Access<'a>>, id: u32, capacity: usize)
        -> Result<(u64, fs::Arguments<'static>), Status>
    {
        let owner = owner.into();
        let (file, provider) = self.provider_file(id)?;
        if !file.authority.matches(file.owner, owner)
        {
            return Err(ACCESS_DENIED);
        }
        let FileFork::Provider(fork) = &*file.fork.lock()
        else
        {
            return Err(BAD_ABI);
        };
        let length = fork.ready_length()?;
        let mut state = fork.state.lock();
        let remaining = length.checked_sub(file.position
                                               .load(core::sync::atomic::Ordering::Relaxed))
                              .ok_or(BAD_ABI)?;
        let count = remaining.min(capacity.min(fs::MAX_TRANSFER) as u64);
        state.phase = Phase::Reading;
        Ok((provider,
            fs::Arguments { volume: file.volume,
                            token: u64::from(id),
                            flags: fork.kind,
                            offset: file.position.load(core::sync::atomic::Ordering::Relaxed),
                            length: count,
                            ..Default::default() }))
    }

    pub(in crate::toolbox::files) fn finish_provider_file_read(&mut self, id: u32, offset: u64,
                                                               count: u64,
                                                               delivered: Option<usize>)
                                                               -> Result<(u64, u64), Status>
    {
        let (file, _) = self.provider_file(id)?;
        let FileFork::Provider(fork) = &*file.fork.lock()
        else
        {
            return Err(BAD_ABI);
        };
        let mut state = fork.state.lock();
        if state.phase != Phase::Reading
           || file.position.load(core::sync::atomic::Ordering::Relaxed) != offset
        {
            return Err(BAD_ABI);
        }
        // Restore tracking even on provider errors or abandoned callers.
        state.phase = Phase::Ready;
        if state.orphaned
        {
            signal_cleanup();
        }
        if let Some(delivered) = delivered
        {
            if delivered as u64 > count || count > state.length.saturating_sub(offset)
            {
                return Err(BAD_ABI);
            }
            file.position.store(offset.checked_add(delivered as u64).ok_or(BAD_ABI)?,
                                core::sync::atomic::Ordering::Relaxed);
        }
        let result = (file.position.load(core::sync::atomic::Ordering::Relaxed), state.length);
        drop(state);
        #[cfg(target_arch = "riscv64")]
        if file.owner_stopping()
        {
            crate::toolbox::processes::retirement::resources_released();
        }
        Ok(result)
    }

    pub(in crate::toolbox::files) fn begin_provider_file_close<'a>(&mut self,
                                                                   owner: impl Into<super::super::subject::Access<'a>>,
                                                                   id: u32)
                                                                   -> Result<(u64, u32), Status>
    {
        let owner = owner.into();
        let (file, provider) = self.provider_file(id)?;
        if !file.authority.matches(file.owner, owner)
        {
            return Err(ACCESS_DENIED);
        }
        let FileFork::Provider(fork) = &*file.fork.lock()
        else
        {
            return Err(BAD_ABI);
        };
        let mut state = fork.state.lock();
        if !matches!(state.phase, Phase::Ready | Phase::Quarantined)
        {
            return Err(SERVICE_BUSY);
        }
        state.phase = Phase::Closing;
        Ok((provider, file.volume))
    }

    pub(in crate::toolbox::files) fn cleanup_provider_file(&mut self, id: u32)
                                                           -> Result<u32, Status>
    {
        let (file, _) = self.provider_file(id)?;
        let FileFork::Provider(fork) = &*file.fork.lock()
        else
        {
            return Err(BAD_ABI);
        };
        let mut state = fork.state.lock();
        if !matches!(state.phase, Phase::Opening | Phase::Ready)
        {
            return Err(SERVICE_BUSY);
        }
        state.orphaned = true;
        state.phase = Phase::Closing;
        Ok(file.volume)
    }

    pub(in crate::toolbox::files) fn quarantine_provider_file(&mut self, id: u32)
                                                              -> Result<(), Status>
    {
        let (file, _) = self.provider_file(id)?;
        let FileFork::Provider(fork) = &*file.fork.lock()
        else
        {
            return Err(BAD_ABI);
        };
        let mut state = fork.state.lock();
        state.phase = Phase::Quarantined;
        Ok(())
    }

    pub(in crate::toolbox::files) fn retire_provider_file(&mut self, id: u32)
                                                          -> Result<(), Status>
    {
        let (file, _) = self.provider_file(id)?;
        let fork_guard = file.fork.lock();
        let FileFork::Provider(fork) = &*fork_guard
        else
        {
            return Err(BAD_ABI);
        };
        let mut state = fork.state.lock();
        if !matches!(state.phase, Phase::Opening | Phase::Closing)
        {
            return Err(SERVICE_BUSY);
        }
        state.phase = Phase::Closed;
        drop(state);
        drop(fork_guard);
        self.handles.remove(id).ok_or(NOT_FOUND)?;
        Ok(())
    }

    pub(in crate::toolbox::files) fn next_orphaned_file(
        &mut self)
        -> Result<Option<(u32, u64, u32, super::super::subject::Subject)>, Status>
    {
        let id = self.handles.iter().find_map(|(id, file)| matches!(&*file.fork.lock(), FileFork::Provider(fork) if fork.cleanup_ready()).then_some(id));
        let Some(id) = id
        else
        {
            return Ok(None);
        };
        let (file, provider) = self.provider_file(id)?;
        let FileFork::Provider(fork) = &*file.fork.lock()
        else
        {
            return Err(BAD_ABI);
        };
        fork.state.lock().phase = Phase::Closing;
        Ok(Some((id, provider, file.volume, file.authority.flight_subject(file.owner))))
    }
}
