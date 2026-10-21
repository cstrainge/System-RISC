//! Generation-checked watchers. Poll is a bounded RAM-only operation; provider
//! probes own their jobs and volume leases until acknowledgement, even on exit.
#![forbid(unsafe_code)]
use super::*;

#[cfg_attr(not(target_arch = "riscv64"), allow(dead_code))]
pub(super) struct Watch
{
    owner:       u64,
    volume:      u32,
    directory:   u32,
    changed:     bool,
    revision:    u64,
    dead:        bool,
    pending:     bool,
    next_due:    u64,
    fingerprint: Option<[u8; 32]>,
    error:       Option<Status>,
    last_error:  Option<Status>
}
impl Manager
{
    pub(super) fn watch_request(&mut self, owner: u64, call: &mut FileCall) -> Result<(), Status>
    {
        if call.index != 0
           || call.position != 0
           || call.length != 0
           || call.offset != 0
           || call.fork != 0
           || call.whence != 0
           || call.reserved != 0
           || call.name_length != 0
           || call.capacity != 0
        {
            return Err(BAD_ARGUMENT);
        }
        match call.operation
        {
            WATCH_DIRECTORY =>
            {
                if call.handle != 0 || call.cnid < ROOT_ID
                {
                    return Err(BAD_ARGUMENT);
                }
                match self.volumes.get(call.volume).ok_or(NOT_FOUND)?
                {
                    Mounted::Block(_) =>
                    {
                        if !self.entry(call.volume, call.cnid)?.directory
                        {
                            return Err(BAD_ARGUMENT);
                        }
                    }
                    Mounted::Provider(m) =>
                    {
                        if m.phase != provider::Phase::Live
                        {
                            return Err(SERVICE_BUSY);
                        }
                    }
                }
                call.handle = self.watches
                                  .insert(Watch { owner,
                                                  volume: call.volume,
                                                  directory: call.cnid,
                                                  changed: false,
                                                  revision: scan::next_revision()?,
                                                  dead: false,
                                                  pending: false,
                                                  next_due: 0,
                                                  fingerprint: None,
                                                  error: None,
                                                  last_error: None })
                                  .map_err(|_| NO_MEMORY)?;
                #[cfg(target_arch = "riscv64")]
                background::wake();
                Ok(())
            }
            POLL_WATCH | DISPOSE_WATCH =>
            {
                if call.volume != 0 || call.cnid != 0
                {
                    return Err(BAD_ARGUMENT);
                }
                let watch =
                    self.watches.get_mut(call.handle).filter(|w| !w.dead).ok_or(NOT_FOUND)?;
                if watch.owner != owner
                {
                    return Err(ACCESS_DENIED);
                }
                if call.operation == DISPOSE_WATCH
                {
                    watch.dead = true;
                    if !watch.pending
                    {
                        self.watches.remove(call.handle);
                    }
                }
                else
                {
                    if let Some(error) = watch.error.take()
                    {
                        return Err(error);
                    }
                    call.index = u32::from(core::mem::take(&mut watch.changed));
                    call.position = watch.revision;
                }
                Ok(())
            }
            _ => Err(NOT_SUPPORTED)
        }
    }

    pub(super) fn notify_directory(&mut self, volume: u32, directory: u32) -> Result<(), Status>
    {
        let revision = scan::next_revision()?;
        for watch in self.watches.values_mut()
        {
            if !watch.dead
               && watch.volume == volume
               && (directory == 0 || watch.directory == directory)
            {
                watch.changed = true;
                watch.revision = revision;
            }
        }
        Ok(())
    }

    pub(super) fn close_watches(&mut self, owner: u64)
    {
        for watch in self.watches.values_mut()
        {
            if watch.owner == owner
            {
                watch.dead = true;
            }
        }
        loop
        {
            let next = self.watches.iter().find_map(|(id, w)| (w.dead && !w.pending).then_some(id));
            let Some(id) = next
            else
            {
                break;
            };
            self.watches.remove(id);
        }
    }
}

#[cfg(target_arch = "riscv64")]
mod background
{
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use mac_filesystem_interface::{Arguments, Operation};
    use spinning_top::Spinlock;

    use super::*;
    use crate::drivers::OwnedJob;
    enum Work
    {
        Polling(OwnedJob),
        Complete(Result<Vec<u8>, Status>),
        Faulted
        {
            _job: OwnedJob
        }
    }
    struct Probe
    {
        watch:  u32,
        volume: u32,
        work:   Work
    }
    static RUNNING: AtomicBool = AtomicBool::new(false);
    struct Running;
    impl Drop for Running
    {
        fn drop(&mut self)
        {
            RUNNING.store(false, Ordering::Release);
        }
    }
    static ACTIVE: Spinlock<Option<Probe>> = Spinlock::new(None);
    static NEXT_CHECK: AtomicU64 = AtomicU64::new(0);
    pub(super) fn wake()
    {
        NEXT_CHECK.store(0, Ordering::Release);
    }
    // MTIME runs at 10 MHz. Each folder is probed at most four times per second;
    // pointer and key event rates have no bearing on this schedule.
    const INTERVAL: u64 = 2_500_000;

    impl Manager
    {
        fn next_probe(&mut self, now: u64) -> Result<Option<(u32, u32, u32, u32)>, Status>
        {
            let next = self.watches.iter().find_map(|(id,w)| {
                if w.dead || w.pending || w.next_due > now { return None; }
                let Mounted::Provider(m) = self.volumes.get(w.volume)? else { return None; };
                (m.changes && m.phase == provider::Phase::Live).then_some((id,w.volume,w.directory))
            });
            let Some((id, volume, directory)) = next
            else
            {
                return Ok(None);
            };
            let provider = self.begin_provider_read(volume)?;
            let w = self.watches.get_mut(id).unwrap();
            w.pending = true;
            w.next_due = now.saturating_add(INTERVAL);
            Ok(Some((id, provider, volume, directory)))
        }

        fn finish_probe(&mut self, id: u32, volume: u32, result: Result<&[u8], Status>)
                        -> Result<(), Status>
        {
            self.end_provider_read(volume)?;
            let result = result.and_then(|bytes| {
                                   if bytes.len() != 33 || bytes[0] > 1
                                   {
                                       return Err(BAD_ABI);
                                   }
                                   Ok((bytes[0] != 0,
                                       <[u8; 32]>::try_from(&bytes[1..]).map_err(|_| BAD_ABI)?))
                               });
            let w = self.watches.get_mut(id).ok_or(NOT_FOUND)?;
            w.pending = false;
            if w.dead
            {
                self.watches.remove(id);
                return Ok(());
            }
            match result
            {
                Ok((reset, stamp)) =>
                {
                    let changed = reset || w.fingerprint.is_some_and(|previous| previous != stamp);
                    w.fingerprint = Some(stamp);
                    w.last_error = None;
                    if changed
                    {
                        w.changed = true;
                        w.revision = scan::next_revision()?;
                        self.provider_mount(volume)?.revision = scan::next_revision()?;
                    }
                }
                // Contention is normal when a foreground writer owns the provider.
                Err(SERVICE_BUSY) => (),
                Err(error) =>
                {
                    if w.last_error != Some(error)
                    {
                        w.error = Some(error);
                    }
                    w.last_error = Some(error);
                }
            }
            Ok(())
        }
    }
    pub(crate) fn pump()
    {
        if RUNNING.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err()
        {
            return;
        }
        let _running = Running;
        let now = crate::platform::ticks();
        let active = ACTIVE.lock().take();
        if active.is_none() && now < NEXT_CHECK.load(Ordering::Acquire)
        {
            return;
        }
        if let Some(mut probe) = active
        {
            probe.work = match probe.work
            {
                Work::Polling(mut job) =>
                {
                    match job.poll()
                    {
                        Ok(None) => Work::Polling(job),
                        Ok(Some(done)) =>
                        {
                            Work::Complete(done.result.map_err(|error| {
                                match error
                                {
                                    crate::drivers::jobs::Failure::Service(error) => error,
                                    crate::drivers::jobs::Failure::Cancelled => OPERATION_CANCELLED
                                }
                            }))
                        }
                        Err(error) =>
                        {
                            // No terminal acknowledgement: keep all ownership and
                            // quarantine the mount rather than recycling its IDs.
                            let _ = crate::kernel::with_files(|m| {
                                m.quarantine_provider_mount(probe.volume)
                            });
                            serial!("FILE_WATCH_JOB_ERROR status={}", error);
                            Work::Faulted { _job: job }
                        }
                    }
                }
                work => work
            };
            match &probe.work
            {
                Work::Complete(result) =>
                {
                    // Access contention must not discard a consumed job result.
                    // Keep those owned bytes until the manager accepts the ACK.
                    if crate::kernel::with_files(|m| {
                           m.finish_probe(probe.watch,
                                          probe.volume,
                                          result.as_deref().map_err(|error| *error))
                       }).is_err()
                    {
                        *ACTIVE.lock() = Some(probe);
                        return;
                    }
                }
                Work::Polling(_) | Work::Faulted { .. } =>
                {
                    *ACTIVE.lock() = Some(probe);
                    return;
                }
            }
        }
        let next = crate::kernel::with_files(|m| m.next_probe(now));
        let Ok(Ok(Some((watch, provider, volume, node)))) = next
        else
        {
            NEXT_CHECK.store(now.saturating_add(INTERVAL), Ordering::Release);
            return;
        };
        let work = match provider::runtime::submit_args(provider,
                                                        Operation::CheckDirectory,
                                                        Arguments { volume,
                                                                    node,
                                                                    ..Default::default() })
        {
            Ok(job) => Work::Polling(job),
            Err(error) => Work::Complete(Err(error))
        };
        *ACTIVE.lock() = Some(Probe { watch,
                                      volume,
                                      work });
    }
}
#[cfg(target_arch = "riscv64")]
pub(crate) use background::pump;
