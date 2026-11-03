//! Coalesced layout work lives with the existing per-owner CTRL storage. Root
//! identities and ownership remain in the canonical object tree; this is not
//! another object registry. No allocation is needed to invalidate or wake work.
use super::*;

const RETRY_TICKS: u64 = 60;
static CLAIM_RETURNS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);
static RETURN_EPOCH_EXHAUSTED: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Availability hint only, called exclusively by the actual last Claim Drop.
/// No wrapping alias or unrelated account/reference release can advance it.
pub(super) fn claim_returned()
{
    use core::sync::atomic::Ordering::Relaxed;
    if CLAIM_RETURNS.try_update(Relaxed, Relaxed, |value| value.checked_add(1)).is_err()
    {
        RETURN_EPOCH_EXHAUSTED.store(true, Relaxed);
    }
}
fn return_epoch() -> Result<u64, Status>
{
    use core::sync::atomic::Ordering::Relaxed;
    if RETURN_EPOCH_EXHAUSTED.load(Relaxed)
    {
        return Err(BAD_ABI);
    }
    Ok(CLAIM_RETURNS.load(Relaxed))
}

#[derive(Clone, Copy, Default)]
enum Wake
{
    #[default]
    Idle,
    Ready,
    Blocked,
    Retry(u64)
}
impl Wake
{
    fn due(self, now: u64) -> bool
    {
        matches!(self, Self::Ready) || matches!(self, Self::Retry(at) if at <= now)
    }
}
#[derive(Default)]
pub(super) struct Automatic
{
    wake:   Wake,
    status: Status
}
#[derive(Default)]
pub(super) struct Work
{
    ready:          bool,
    retry:          Option<u64>,
    // Readiness coalescing only. A canceled envelope expires this Weak without
    // requiring Desktop in Drop; original root wakes remain authoritative.
    batch:          alloc::sync::Weak<AutomaticBatchLease>,
    observed_epoch: u64,
    blocked:        bool,
    return_pending: bool
}
/// Affine original-event batch lifetime, not callback/paint/stop authority.
pub(crate) struct AutomaticBatchLease
{
    owner:  mac_object_table::domain::DomainId,
    image:  crate::images::Binding,
    thread: crate::context::ThreadId
}
impl AutomaticBatchLease
{
    pub(crate) fn prepare(owner: &OwnerRef, image: crate::images::Binding,
                          thread: crate::context::ThreadId,
                          allocation: &crate::machine::scheduler::accounting::AllocationAuthority)
                          -> Result<Arc<Self>, Status>
    {
        if owner.owner() != allocation.owner()
           || image.owner() != owner.owner().incarnation()
           || crate::context::current_thread_context().identity().thread != thread
        {
            return Err(ACCESS_DENIED);
        }
        let target =
            crate::allocator::Target::Owner { account: allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || {
            Arc::try_new(Self { owner: owner.owner(),
                                image,
                                thread }).map_err(|_| NO_MEMORY)
        })
    }

    pub(crate) fn matches(&self, owner: &OwnerRef, image: crate::images::Binding) -> bool
    {
        self.owner == owner.owner()
        && self.image == image
        && self.thread == crate::context::current_thread_context().identity().thread
    }
}
#[derive(Clone, Copy)]
pub(crate) struct Root
{
    pub(crate) id:     u64,
    pub(crate) window: u64
}
impl Manager
{
    pub(crate) fn automatic_root_needs_user(&self, root: Root, owner: u64) -> Result<bool, Status>
    {
        let original = self.get(root.id, owner)?;
        if original.window != root.window || !Self::automatic_root(original)
        {
            return Err(BAD_ARGUMENT);
        }
        let mut id = root.id;
        loop
        {
            let object = self.get(id, owner)?;
            if object.definition.is_process_source() || object.state.process_dependencies()
            {
                return Ok(true);
            }
            id = self.next_in(root.id, id);
            if id == 0
            {
                return Ok(false);
            }
        }
    }

    pub(crate) fn user_automatic_ready(&mut self, owner: &OwnerRef, now: u64)
                                       -> Result<bool, Status>
    {
        let Some(work) = self.objects.state_mut(owner.owner().incarnation())
        else
        {
            return Ok(false);
        };
        if work.batch.strong_count() != 0
        {
            return Ok(false);
        }
        if work.blocked
        {
            let epoch = return_epoch()?;
            if epoch != work.observed_epoch
            {
                work.return_pending = true;
            }
        }
        let returned = work.return_pending;
        if !work.ready && !work.retry.is_some_and(|at| at <= now) && !returned
        {
            return Ok(false);
        }
        for (id, object) in self.objects
                                .iter_owner(owner)
                                .map_err(pool_status)?
                                .filter(|(_, object)| {
                                    object.recipient == owner.owner().incarnation()
                                    && Self::automatic_root(object)
                                    && (object.automatic.wake.due(now)
                                        || (returned
                                            && matches!(object.automatic.wake, Wake::Blocked)))
                                    && object.state
                                             .layout
                                             .as_ref()
                                             .is_some_and(|layout| layout.arrangement_pending)
                                })
        {
            if returned
               || self.automatic_root_needs_user(Root { id,
                                                        window: object.window },
                                                 owner.owner().incarnation())?
            {
                return Ok(true);
            }
        }
        // No matching pending root remains (including original full-key
        // retirement). Do not retain a stale hint and scan on clean polls.
        self.finish_layout_work(owner.owner().incarnation());
        Ok(false)
    }

    pub(crate) fn automatic_admission_refused(&mut self, owner: u64, now: u64)
    {
        let returned = self.objects.state(owner).is_some_and(|work| work.return_pending);
        for object in self.objects.values_mut().filter(|object| {
                                                   object.recipient == owner
                                                   && Self::automatic_root(object)
                                                   && (object.automatic.wake.due(now)
                                                       || (returned
                                                           && matches!(object.automatic.wake,
                                                                       Wake::Blocked)))
                                               })
        {
            object.automatic.status = NO_MEMORY;
            object.automatic.wake = Wake::Retry(now.saturating_add(RETRY_TICKS));
        }
        if let Some(work) = self.objects.state_mut(owner)
        {
            work.return_pending = false;
        }
        self.finish_layout_work(owner);
    }

    pub(crate) fn capture_automatic_batch(
        &mut self, owner: &OwnerRef, lease: &Arc<AutomaticBatchLease>, now: u64)
        -> Result<Option<(Vec<Root>, alloc::sync::Weak<AutomaticBatchLease>)>, Status>
    {
        if !owner.accepting()
           || lease.owner != owner.owner()
           || lease.thread != crate::context::current_thread_context().identity().thread
        {
            return Err(ACCESS_DENIED);
        }
        if !self.user_automatic_ready(owner, now)?
        {
            return Ok(None);
        }
        let observed_epoch = return_epoch()?;
        let returned =
            self.objects.state(owner.owner().incarnation()).is_some_and(|work| work.return_pending);
        let mut roots = Vec::new();
        for (id, object) in self.objects
                                .iter_owner(owner)
                                .map_err(pool_status)?
                                .filter(|(_, object)| {
                                    object.recipient == owner.owner().incarnation()
                                    && Self::automatic_root(object)
                                    && (object.automatic.wake.due(now)
                                        || (returned
                                            && matches!(object.automatic.wake, Wake::Blocked)))
                                    && object.state
                                             .layout
                                             .as_ref()
                                             .is_some_and(|layout| layout.arrangement_pending)
                                })
        {
            if self.objects.owner(id) != Some(owner.owner())
            {
                continue;
            }
            roots.try_reserve(1).map_err(|_| NO_MEMORY)?;
            roots.push(Root { id,
                              window: object.window });
        }
        let work = self.objects.state_mut(owner.owner().incarnation()).ok_or(NOT_FOUND)?;
        // No original root readiness is consumed by lease installation. Both
        // failed capture and canceled envelopes leave the existing work pending.
        work.observed_epoch = observed_epoch;
        let retired = core::mem::replace(&mut work.batch, Arc::downgrade(lease));
        Ok(Some((roots, retired)))
    }

    fn root(&self, mut id: u64) -> Option<u64>
    {
        loop
        {
            let o = self.objects.get(id)?;
            if o.tree.parent == 0
            {
                return Some(id);
            }
            id = o.tree.parent;
        }
    }

    fn automatic_root(o: &Object) -> bool
    {
        o.tree.parent == 0
        && o.window != 0
        && o.state.layout.as_ref().is_some_and(|l| {
                                      l.anchors != layout::Anchors::NONE
                                      || matches!(o.state.spec,
                                                  Spec::Standard { procedure: TABLE_PROCEDURE,
                                                                   .. })
                                  })
    }

    pub(super) fn schedule_layout(&mut self, id: u64)
    {
        let Some(root) = self.root(id)
        else
        {
            return;
        };
        let Some(o) = self.objects.get(root)
        else
        {
            return;
        };
        if !Self::automatic_root(o)
           || !o.state.layout.as_ref().is_some_and(|l| l.arrangement_pending)
        {
            return;
        }
        let owner = o.recipient;
        // Protected modal objects require their actual Activity. Ordinary
        // application event polling must never borrow System recovery authority.
        if self.objects.owner(root).is_none_or(|storage| storage.incarnation() != owner)
        {
            return;
        }
        if let Some(o) = self.objects.get_mut(root)
        {
            o.automatic.wake = Wake::Ready;
        }
        if let Some(work) = self.objects.state_mut(owner)
        {
            work.ready = true;
        }
    }

    pub(super) fn layout_committed(&mut self, id: u64)
    {
        if let Some(o) = self.objects.get_mut(id)
           && !o.state.layout.as_ref().is_some_and(|l| l.arrangement_pending)
        {
            o.automatic = Automatic::default();
        }
    }

    pub(super) fn layout_status(&self, id: u64) -> Status
    {
        self.root(id).and_then(|root| self.objects.get(root)).map_or(OK, |o| o.automatic.status)
    }

    /// Capture release wakes only its affected root. No measurement or drawing
    /// is performed in the cancellation path.
    pub(super) fn wake_capture(&mut self, id: u64)
    {
        let Some(root) = self.root(id)
        else
        {
            return;
        };
        if self.objects.get(root).is_some_and(|o| matches!(o.automatic.wake, Wake::Blocked))
        {
            self.schedule_layout(root);
        }
    }

    /// Ending a paint transaction also releases retained storage pressure. Wake
    /// only blocked/retrying roots in that window, at the next event boundary.
    pub(crate) fn layout_scene_changed(&mut self, owner: u64, window: u64)
    {
        let mut ready = false;
        for o in self.objects.values_mut().filter(|o| {
                                              o.recipient == owner
                                              && o.window == window
                                              && Self::automatic_root(o)
                                          })
        {
            if matches!(o.automatic.wake, Wake::Blocked | Wake::Retry(_))
            {
                o.automatic.wake = Wake::Ready;
                ready = true;
            }
        }
        if ready && let Some(work) = self.objects.state_mut(owner)
        {
            work.ready = true;
        }
    }

    /// A clean event poll reads only this owner's inline work state. The
    /// temporary ID list is admitted once per batch, never once per setter.
    pub(crate) fn take_layout_work(&mut self, owner: u64, now: u64)
                                   -> Result<Option<Vec<Root>>, Status>
    {
        let Some(work) = self.objects.state_mut(owner)
        else
        {
            return Ok(None);
        };
        if work.batch.strong_count() != 0
        {
            return Ok(None);
        }
        if !work.ready && !work.retry.is_some_and(|at| at <= now)
        {
            return Ok(None);
        }
        work.ready = false;
        work.retry = None;
        let mut roots = Vec::new();
        let admitted = (|| -> Result<(), Status> {
            for (id, o) in self.objects.iter().filter(|(_, o)| {
                                                  o.recipient == owner
                                                  && Self::automatic_root(o)
                                                  && o.automatic.wake.due(now)
                                                  && o.state
                                                      .layout
                                                      .as_ref()
                                                      .is_some_and(|l| l.arrangement_pending)
                                              })
            {
                roots.try_reserve(1).map_err(|_| NO_MEMORY)?;
                roots.push(Root { id,
                                  window: o.window });
            }
            Ok(())
        })();
        if let Err(status) = admitted
        {
            // Existing root readiness survives actual scratch refusal.
            self.automatic_admission_refused(owner, now);
            return Err(status);
        }
        Ok(Some(roots))
    }

    pub(crate) fn finish_layout_work(&mut self, owner: u64)
    {
        let mut ready = false;
        let mut retry: Option<u64> = None;
        let mut blocked = false;
        for (_, o) in
            self.objects.iter().filter(|(_, o)| {
                                   o.recipient == owner
                                   && Self::automatic_root(o)
                                   && o.state.layout.as_ref().is_some_and(|l| l.arrangement_pending)
                               })
        {
            match o.automatic.wake
            {
                Wake::Ready => ready = true,
                Wake::Retry(at) => retry = Some(retry.map_or(at, |old| old.min(at))),
                Wake::Blocked => blocked = true,
                _ => ()
            }
        }
        if let Some(work) = self.objects.state_mut(owner)
        {
            if work.batch.strong_count() != 0
            {
                // A full owned batch settled its captured attempts. A real
                // return during that batch remains eligible; cancel never
                // reaches this consumption point.
                work.return_pending =
                    blocked && return_epoch().map_or(true, |epoch| epoch != work.observed_epoch);
            }
            if !blocked
            {
                work.return_pending = false;
            }
            work.blocked = blocked;
            work.ready = ready;
            work.retry = retry;
        }
    }

    /// Return whether the failure changed, for bounded transition diagnostics.
    pub(crate) fn layout_refused(&mut self, id: u64, status: Status, now: u64) -> bool
    {
        let Some(o) = self.objects.get_mut(id)
        else
        {
            return false;
        };
        let changed = o.automatic.status != status;
        o.automatic.status = status;
        o.automatic.wake = match status
        {
            SERVICE_BUSY => Wake::Blocked,
            NO_MEMORY => Wake::Retry(now.saturating_add(RETRY_TICKS)),
            _ => Wake::Idle
        };
        let owner = o.recipient;
        if status == SERVICE_BUSY
           && let Some(work) = self.objects.state_mut(owner)
        {
            work.blocked = true;
        }
        changed
    }

    pub(crate) fn layout_wait_ticks(&self, owner: u64, now: u64, requested: u32) -> u32
    {
        self.objects.state(owner).and_then(|work| work.retry).map_or(requested, |at| {
            requested.min(at.saturating_sub(now).max(1).min(u64::from(u32::MAX)) as u32)
        })
    }

    pub(crate) fn prepare_automatic(&self, root: Root, owner: u64, viewport: Rect,
                                    paint_pending: bool, gate: TrapGate)
                                    -> Result<Prepared, Status>
    {
        let snapshot = self.snapshot_automatic(root, owner, viewport, paint_pending)?;
        let author = super::super::effects::Author::trusted(gate);
        super::super::effects::immediate(snapshot.calculate(&author))
    }

    pub(super) fn snapshot_automatic(&self, root: Root, owner: u64, viewport: Rect,
                                     paint_pending: bool)
                                     -> Result<grid::LayoutSnapshot, Status>
    {
        let o = self.get(root.id, owner)?;
        if !Self::automatic_root(o) || o.window != root.window
        {
            return Err(BAD_ARGUMENT);
        }
        if paint_pending
        {
            return Err(SERVICE_BUSY);
        }
        // Check capture before allocating a plan or loading definitions. A
        // deferred poll must not disturb capture or spend layout scratch.
        let mut id = root.id;
        loop
        {
            if self.get(id, owner)?.state.tracking.is_some()
            {
                return Err(SERVICE_BUSY);
            }
            id = self.next_in(root.id, id);
            if id == 0
            {
                break;
            }
        }
        let l = o.state.layout.as_ref().ok_or(NOT_SUPPORTED)?;
        let window =
            (l.anchors != layout::Anchors::NONE).then_some((viewport, l.anchors, l.window_border));
        let call = ControlCall { id: root.id, operation: REFLOW_OBJECT,
            layout: mac_abi::controls::reflow::Payload {
                message: mac_abi::controls::reflow::ASSIGN_RECT,
                rect: window.map_or(o.state.spec.bounds(), |(rect, _, _)| rect),
                ..Default::default()
            }, ..Default::default() };
        self.snapshot_layout_change(&call, owner, false, window, None, None)
    }
}

impl State
{
    /// Original admitted auxiliary/capture sources, never current selection.
    pub(super) fn process_dependencies(&self) -> bool
    {
        self.layout.as_ref().is_some_and(|layout| {
                                layout.scroll
                                      .as_ref()
                                      .is_some_and(|scroll| scroll.definition.is_process_source())
                                || layout.grid
                                         .as_ref()
                                         .is_some_and(|grid| grid.has_process_source())
                                || layout.outlines
                                         .as_ref()
                                         .and_then(|grid| grid.dividers.as_ref())
                                         .is_some_and(|dividers| {
                                             dividers.definition.is_process_source()
                                         })
                            })
        || self.splitter.as_ref().is_some_and(|capture| capture.has_process_source())
    }
}
