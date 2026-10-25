//! Generation-checked watchers. Poll is a bounded RAM-only operation; provider
//! probes own their jobs and volume leases until acknowledgement, even on exit.
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use mac_object_table::{domain::{CompletionError, ReleasedResource},
                       projected};

use super::*;
use crate::toolbox::handles::{InvocationGate, KindLease, OwnerRef};
pub(crate) mod lifetime;

struct ProbeState
{
    owner:  OwnerRef,
    active: AtomicBool
}
pub(crate) struct ProbeLease
{
    watch:     u32,
    volume:    u32,
    directory: u32,
    state:     Arc<ProbeState>
}
impl Drop for ProbeLease
{
    fn drop(&mut self)
    {
        assert!(!self.state.active.load(Ordering::Acquire),
                "directory probe lease dropped before terminal acknowledgement");
    }
}

type Retiring = projected::Removed<Watch, InvocationGate>;
pub(super) struct Watches
{
    objects:  Option<projected::Store<Watch, InvocationGate>>,
    retiring: Vec<Retiring>
}
impl Watches
{
    pub(super) const fn new() -> Self
    {
        Self { objects:  None,
               retiring: Vec::new() }
    }

    pub(super) fn prepare(&mut self, kinds: (KindLease, KindLease)) -> Result<(), Status>
    {
        if self.objects.is_none()
        {
            self.objects = Some(projected::Store::new(Kind::FileWatches, kinds.0, kinds.1)
                .map_err(crate::toolbox::handles::projection_status)?);
        }
        Ok(())
    }

    fn insert(&mut self, owner: &OwnerRef, watch: Watch) -> Result<u32, Status>
    {
        let live = self.iter().count();
        let objects = self.objects.as_mut().ok_or(NOT_SUPPORTED)?;
        // Reserve cleanup capacity before publication, never while the owner
        // walk is retaining a provider probe's already-invalidated payload.
        self.retiring.try_reserve(live + 1).map_err(|_| NO_MEMORY)?;
        objects.insert(owner, watch).map_err(crate::toolbox::handles::projection_status)
    }

    fn get_mut(&mut self, id: u32) -> Option<&mut Watch>
    {
        self.objects.as_mut()?.get_mut(id)
    }

    fn owner(&self, id: u32) -> Option<u64>
    {
        self.objects.as_ref()?.get_with_owner(id).map(|(owner, _)| owner.incarnation())
    }

    fn iter(&self) -> impl Iterator<Item = (u32, &Watch)>
    {
        self.objects.iter().flat_map(|objects| objects.iter())
    }

    fn values_mut(&mut self) -> impl Iterator<Item = &mut Watch>
    {
        self.objects.iter_mut().flat_map(|objects| objects.values_mut())
    }

    fn retain(&mut self, guard: Retiring)
    {
        if guard.pending()
        {
            assert!(self.retiring.len() < self.retiring.capacity(),
                    "watch cleanup was not reserved before publication");
            self.retiring.push(guard);
        }
        else
        {
            drop(guard);
        }
    }

    fn remove(&mut self, id: u32) -> Option<()>
    {
        let guard = self.objects.as_mut()?.remove(id)?;
        self.retain(guard);
        Some(())
    }

    fn release(&mut self, ticket: ReleasedResource, storage: bool) -> Result<(), CompletionError>
    {
        let Some(objects) = self.objects.as_mut()
        else
        {
            return Err(<CompletionError as mac_object_table::registry::CleanupFailure>::registration_mismatch(ticket));
        };
        if storage
        {
            return objects.release_storage(ticket);
        }
        let guard = objects.release(ticket)?;
        self.retain(guard);
        Ok(())
    }

    fn finish_released(&mut self, lease: &ProbeLease) -> bool
    {
        let Some(index) = self.retiring.iter().position(|guard| {
                                                  guard.wire() == lease.watch
            && guard.probe.as_ref().is_some_and(|state| Arc::ptr_eq(state, &lease.state))
                                              })
        else
        {
            return false;
        };
        drop(self.retiring.swap_remove(index));
        true
    }
}
impl Watches
{
    fn validate_lease<'a>(&'a self, lease: &ProbeLease) -> Result<Option<&'a Watch>, Status>
    {
        if !lease.state.active.load(Ordering::Acquire)
        {
            return Err(BAD_ARGUMENT);
        }
        let Some((owner, watch)) =
            self.objects.as_ref().and_then(|objects| objects.get_with_owner(lease.watch))
        else
        {
            return Ok(None);
        };
        if owner != lease.state.owner.owner()
           || watch.volume != lease.volume
           || watch.directory != lease.directory
           || !watch.probe.as_ref().is_some_and(|state| Arc::ptr_eq(state, &lease.state))
        {
            return Err(BAD_ABI);
        }
        Ok(Some(watch))
    }

    fn acknowledge(&mut self, lease: &ProbeLease)
    {
        assert!(lease.state.active.swap(false, Ordering::AcqRel), "probe acknowledged twice");
        if self.get_mut(lease.watch).is_none()
        {
            self.finish_released(lease);
            #[cfg(target_arch = "riscv64")]
            if !lease.state.owner.accepting()
            {
                crate::toolbox::processes::retirement::resources_released();
            }
        }
    }
}
impl Drop for Watches
{
    fn drop(&mut self)
    {
        // Abandonment cannot acknowledge an external probe. A resident manager
        // must stay owned through its death barrier; preserve a visible pending
        // charge rather than recycling its lifetime if that contract is broken.
        if !self.retiring.is_empty()
        {
            core::mem::forget(core::mem::take(&mut self.retiring));
        }
    }
}

#[cfg_attr(not(target_arch = "riscv64"), allow(dead_code))]
pub(super) struct Watch
{
    volume:            u32,
    directory:         u32,
    changed:           bool,
    revision:          u64,
    deferred_revision: Option<u64>,
    probe:             Option<Arc<ProbeState>>,
    next_due:          u64,
    fingerprint:       Option<[u8; 32]>,
    error:             Option<Status>,
    last_error:        Option<Status>
}
impl Watch
{
    fn pending(&self) -> bool
    {
        self.probe.as_ref().is_some_and(|state| state.active.load(Ordering::Acquire))
    }
}
impl Manager
{
    #[cfg(all(feature = "test-support", not(target_arch = "riscv64")))]
    pub(crate) fn begin_fixture_probe(&mut self, owner: &OwnerRef, id: u32)
                                      -> Result<ProbeLease, Status>
    {
        if self.watches.owner(id).ok_or(NOT_FOUND)? != owner.owner().incarnation()
        {
            return Err(ACCESS_DENIED);
        }
        let watch = self.watches.get_mut(id).ok_or(NOT_FOUND)?;
        if watch.pending()
        {
            return Err(SERVICE_BUSY);
        }
        if watch.probe.is_none()
        {
            watch.probe =
                Some(Arc::try_new(ProbeState { owner:  owner.clone(),
                                               active: AtomicBool::new(false) }).map_err(|_| {
                                                                                    NO_MEMORY
                                                                                })?);
        }
        let state = watch.probe.as_ref().unwrap().clone();
        state.active.store(true, Ordering::Release);
        Ok(ProbeLease { watch: id,
                        volume: watch.volume,
                        directory: watch.directory,
                        state })
    }

    #[cfg(all(feature = "test-support", not(target_arch = "riscv64")))]
    pub(crate) fn finish_fixture_probe(&mut self, lease: &ProbeLease) -> Result<(), Status>
    {
        let _ = self.watches.validate_lease(lease)?;
        self.watches.acknowledge(lease);
        Ok(())
    }

    pub(crate) fn prepare_watches(&mut self, kinds: (KindLease, KindLease)) -> Result<(), Status>
    {
        self.watches.prepare(kinds)
    }

    pub(crate) fn watch_request(&mut self, caller: &Caller, call: &mut FileCall)
                                -> Result<(), Status>
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
                if !caller.owner.accepting()
                {
                    return Err(SERVICE_BUSY);
                }
                if call.handle != 0 || call.cnid < ROOT_ID
                {
                    return Err(BAD_ARGUMENT);
                }
                let probes = match self.volumes.get(call.volume).ok_or(NOT_FOUND)?
                {
                    Mounted::Block(_) =>
                    {
                        if !self.entry(call.volume, call.cnid)?.directory
                        {
                            return Err(BAD_ARGUMENT);
                        }
                        false
                    }
                    Mounted::Provider(m) =>
                    {
                        if m.phase != provider::Phase::Live
                        {
                            return Err(SERVICE_BUSY);
                        }
                        m.changes
                    }
                };
                let probe = if probes
                {
                    Some(Arc::try_new(ProbeState { owner: caller.owner.clone(), active: AtomicBool::new(false) }).map_err(|_| NO_MEMORY)?)
                }
                else
                {
                    None
                };
                self.prepare_watches(caller.kinds.clone().ok_or(NOT_SUPPORTED)?)?;
                call.handle = self.watches.insert(&caller.owner,
                                                   Watch { volume: call.volume,
                                                           directory: call.cnid,
                                                           changed: false,
                                                           revision: scan::next_revision()?,
                                                           deferred_revision: None,
                                                           probe,
                                                           next_due: 0,
                                                           fingerprint: None,
                                                           error: None,
                                                           last_error: None })?;
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
                if self.watches.owner(call.handle).ok_or(NOT_FOUND)?
                   != caller.owner.owner().incarnation()
                {
                    return Err(ACCESS_DENIED);
                }
                if call.operation == DISPOSE_WATCH
                {
                    self.watches.remove(call.handle).ok_or(NOT_FOUND)?;
                }
                else
                {
                    let watch = self.watches.get_mut(call.handle).ok_or(NOT_FOUND)?;
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

    fn transfer_holds_notices(&self, volume: u32) -> bool
    {
        #[cfg(target_arch = "riscv64")]
        {
            self.transfers.iter().any(|(_, entry)| entry.holds_notices(volume))
        }
        #[cfg(not(target_arch = "riscv64"))]
        {
            let _ = volume;
            false
        }
    }

    #[cfg(target_arch = "riscv64")]
    pub(super) fn release_transfer_notices(&mut self)
    {
        let transfers = &self.transfers;
        for watch in self.watches.values_mut()
        {
            if watch.deferred_revision.is_some()
               && !transfers.iter().any(|(_, entry)| entry.holds_notices(watch.volume))
            {
                watch.changed = true;
                watch.revision = watch.deferred_revision.take().unwrap();
                watch.fingerprint = None;
            }
        }
    }

    pub(super) fn notify_directory(&mut self, volume: u32, directory: u32) -> Result<(), Status>
    {
        let revision = scan::next_revision()?;
        self.notify_directory_at(volume, directory, revision);
        Ok(())
    }

    pub(super) fn notify_directory_at(&mut self, volume: u32, directory: u32, revision: u64)
    {
        #[cfg(any(target_arch = "riscv64", feature = "test-support"))]
        self.file_icons.invalidate(volume, directory);
        let held = self.transfer_holds_notices(volume);
        for watch in self.watches.values_mut()
        {
            if watch.volume == volume && (directory == 0 || watch.directory == directory)
            {
                if held
                {
                    watch.deferred_revision = Some(revision);
                }
                else
                {
                    watch.changed = true;
                    watch.revision = revision;
                }
            }
        }
    }

    /// A transaction reserves this revision before its first side effect. One
    /// sorted parent set visits the bounded watch table once, without allocation
    /// or a fallible counter increment after publication.
    pub(super) fn notify_group_at(&mut self, volume: u32, group: u64, revision: u64)
    {
        let held = self.transfer_holds_notices(volume);
        let Some(mark) = self.groups.completing(group)
        else
        {
            return;
        };
        #[cfg(any(target_arch = "riscv64", feature = "test-support"))]
        for &parent in &mark.parents
        {
            self.file_icons.invalidate(volume, parent);
        }
        for watch in self.watches.values_mut()
        {
            if watch.volume == volume && mark.parents.binary_search(&watch.directory).is_ok()
            {
                if held
                {
                    watch.deferred_revision = Some(revision);
                }
                else
                {
                    watch.changed = true;
                    watch.revision = revision;
                }
            }
        }
    }
}

impl lifetime::WatchHost for Manager
{
    fn release_file_watch(&mut self, ticket: ReleasedResource, storage: bool)
                          -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        self.watches.release(ticket, storage).map_err(Into::into)
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
        lease: ProbeLease,
        work:  Work
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
    static HAS_ACTIVE: AtomicBool = AtomicBool::new(false);
    // Faults retain their job/ACK and volume lease without starving unrelated
    // mounts. Capacity is reserved before starting each provider operation.
    static FAULTED: Spinlock<Vec<Probe>> = Spinlock::new(Vec::new());
    static NEXT_CHECK: AtomicU64 = AtomicU64::new(0);
    fn park(probe: Probe)
    {
        *ACTIVE.lock() = Some(probe);
        HAS_ACTIVE.store(true, Ordering::Release);
    }
    fn retain_fault(probe: Probe)
    {
        let mut retained = FAULTED.lock();
        assert!(retained.len() < retained.capacity(),
                "probe fault capacity was not reserved before submission");
        retained.push(probe);
    }

    pub(super) fn wake()
    {
        NEXT_CHECK.store(0, Ordering::Release);
    }
    // MTIME runs at 10 MHz. Each folder is probed at most four times per second;
    // pointer and key event rates have no bearing on this schedule.
    const INTERVAL: u64 = 2_500_000;

    impl Manager
    {
        fn next_probe(&mut self, now: u64) -> Result<Option<(ProbeLease, u32)>, Status>
        {
            let next = self.watches.iter().find_map(|(id,w)| {
                if w.pending() || w.next_due > now || self.transfer_holds_notices(w.volume)
                    || self.groups.iter().any(|(_, group)| group.volume == w.volume)
                { return None; }
                let Mounted::Provider(m) = self.volumes.get(w.volume)? else { return None; };
                (m.changes && m.phase == provider::Phase::Live).then_some((id,w.volume,w.directory))
            });
            let Some((id, volume, directory)) = next
            else
            {
                return Ok(None);
            };
            if FAULTED.lock().try_reserve(1).is_err()
            {
                let watch = self.watches.get_mut(id).ok_or(NOT_FOUND)?;
                if watch.last_error != Some(NO_MEMORY)
                {
                    watch.error = Some(NO_MEMORY);
                }
                watch.last_error = Some(NO_MEMORY);
                watch.next_due = now.saturating_add(INTERVAL);
                return Err(NO_MEMORY);
            }
            let state =
                self.watches.get_mut(id).ok_or(NOT_FOUND)?.probe.as_ref().ok_or(BAD_ABI)?.clone();
            let provider = match self.begin_provider_read(volume)
            {
                Ok(provider) => provider,
                Err(error) =>
                {
                    let watch = self.watches.get_mut(id).ok_or(NOT_FOUND)?;
                    watch.next_due = now.saturating_add(INTERVAL);
                    if error != SERVICE_BUSY
                    {
                        if watch.last_error != Some(error)
                        {
                            watch.error = Some(error);
                        }
                        watch.last_error = Some(error);
                    }
                    return Err(error);
                }
            };
            let Some(watch) = self.watches.get_mut(id)
            else
            {
                self.end_provider_read(volume)?;
                return Err(NOT_FOUND);
            };
            state.active.store(true, Ordering::Release);
            watch.next_due = now.saturating_add(INTERVAL);
            Ok(Some((ProbeLease { watch: id,
                                  volume,
                                  directory,
                                  state },
                     provider)))
        }

        fn finish_probe(&mut self, lease: &ProbeLease, result: Result<&[u8], Status>)
                        -> Result<(), Status>
        {
            let result = result.and_then(|bytes| {
                                   if bytes.len() != 33 || bytes[0] > 1
                                   {
                                       return Err(BAD_ABI);
                                   }
                                   Ok((bytes[0] != 0,
                                       <[u8; 32]>::try_from(&bytes[1..]).map_err(|_| BAD_ABI)?))
                               });
            let changed = self.watches.validate_lease(lease)?.is_some_and(|watch| {
                matches!(result, Ok((reset, stamp)) if reset || watch.fingerprint.is_some_and(|previous| previous != stamp))
            });
            let held = changed && self.transfer_holds_notices(lease.volume);
            // Every fallible preflight precedes lease completion and watch
            // mutation. A refused acknowledgement can therefore stay retained.
            let revisions =
                if changed { Some((scan::next_revision()?, scan::next_revision()?)) } else { None };
            // Hold the validated mount borrow through ACK and publication;
            // never re-resolve a preflighted lease after releasing it.
            let (volumes, watches) = (&mut self.volumes, &mut self.watches);
            let Some(Mounted::Provider(mounted)) = volumes.get_mut(lease.volume)
            else
            {
                return Err(NOT_FOUND);
            };
            mounted.end_read()?;
            watches.acknowledge(lease);
            let Some(watch) = watches.get_mut(lease.watch)
            else
            {
                // Metadata may have been invalidated before its callback could
                // enter Files. The retained state also lets that callback see
                // the terminal ACK on a later retry, without an ordinary ID.
                return Ok(());
            };
            match result
            {
                Ok((_, stamp)) =>
                {
                    watch.fingerprint = Some(stamp);
                    watch.last_error = None;
                    if let Some((revision, mount_revision)) = revisions
                    {
                        if held
                        {
                            watch.deferred_revision = Some(revision);
                        }
                        else
                        {
                            watch.changed = true;
                            watch.revision = revision;
                        }
                        mounted.revision = mount_revision;
                    }
                }
                Err(SERVICE_BUSY) => (),
                Err(error) =>
                {
                    if watch.last_error != Some(error)
                    {
                        watch.error = Some(error);
                    }
                    watch.last_error = Some(error);
                }
            }
            Ok(())
        }

        fn fail_probe(&mut self, lease: &ProbeLease) -> Result<(), Status>
        {
            self.quarantine_provider_mount(lease.volume)?;
            if let Some(watch) = self.watches.get_mut(lease.watch)
            {
                if watch.last_error != Some(ROLLBACK_FAILED)
                {
                    watch.error = Some(ROLLBACK_FAILED);
                }
                watch.last_error = Some(ROLLBACK_FAILED);
            }
            Ok(())
        }
    }
    pub(crate) fn pump()
    {
        let now = crate::platform::ticks();
        if !HAS_ACTIVE.load(Ordering::Acquire) && now < NEXT_CHECK.load(Ordering::Acquire)
        {
            return;
        }
        if RUNNING.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err()
        {
            return;
        }
        let _running = Running;
        let active = ACTIVE.lock().take();
        HAS_ACTIVE.store(false, Ordering::Release);
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
                            let _ = crate::kernel::with_files(|m| m.fail_probe(&probe.lease));
                            serial!("FILE_WATCH_JOB_ERROR owner={} watch={} volume={} \
                                     directory={} status={error}; job and lifetime retained, \
                                     mount quarantined",
                                    probe.lease.state.owner.owner().incarnation(),
                                    probe.lease.watch,
                                    probe.lease.volume,
                                    probe.lease.directory);
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
                    match crate::kernel::with_files(|m| {
                              m.finish_probe(&probe.lease,
                                             result.as_deref().map_err(|error| *error))
                          })
                    {
                        Ok(Ok(())) => (),
                        Err(_) =>
                        {
                            park(probe);
                            return;
                        }
                        Ok(Err(error)) =>
                        {
                            let _ = crate::kernel::with_files(|m| m.fail_probe(&probe.lease));
                            serial!("FILE_WATCH_ACK_ERROR watch={} volume={} status={error}; \
                                     acknowledgement and lifetime retained, mount quarantined",
                                    probe.lease.watch,
                                    probe.lease.volume);
                            retain_fault(probe);
                        }
                    }
                }
                Work::Polling(_) =>
                {
                    park(probe);
                    return;
                }
                Work::Faulted { .. } =>
                {
                    retain_fault(probe);
                }
            }
        }
        let next = crate::kernel::with_files(|m| m.next_probe(now));
        let Ok(Ok(Some((lease, provider)))) = next
        else
        {
            NEXT_CHECK.store(now.saturating_add(INTERVAL), Ordering::Release);
            return;
        };
        let work = match provider::runtime::submit_args(provider,
                                                        Operation::CheckDirectory,
                                                        Arguments { volume: lease.volume,
                                                                    node: lease.directory,
                                                                    ..Default::default() })
        {
            Ok(job) => Work::Polling(job),
            Err(error) => Work::Complete(Err(error))
        };
        park(Probe { lease,
                     work });
    }
}
#[cfg(target_arch = "riscv64")]
pub(crate) use background::pump;
