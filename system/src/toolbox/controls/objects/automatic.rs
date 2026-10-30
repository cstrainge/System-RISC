//! Coalesced layout work lives with the existing per-owner CTRL storage. Root
//! identities and ownership remain in the canonical object tree; this is not
//! another object registry. No allocation is needed to invalidate or wake work.
use super::*;

const RETRY_TICKS: u64 = 60;

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
    ready: bool,
    retry: Option<u64>
}
#[derive(Clone, Copy)]
pub(crate) struct Root
{
    pub(crate) id:     u64,
    pub(crate) window: u64
}
impl Manager
{
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
            // Preserve all requests when even the bounded batch cannot be
            // admitted. No IDs, geometry or pending work are discarded.
            for o in self.objects.values_mut().filter(|o| {
                                                  o.recipient == owner
                                                  && Self::automatic_root(o)
                                                  && o.automatic.wake.due(now)
                                              })
            {
                o.automatic.status = status;
                o.automatic.wake = Wake::Retry(now.saturating_add(RETRY_TICKS));
            }
            self.finish_layout_work(owner);
            return Err(status);
        }
        Ok(Some(roots))
    }

    pub(crate) fn finish_layout_work(&mut self, owner: u64)
    {
        let mut ready = false;
        let mut retry: Option<u64> = None;
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
                _ => ()
            }
        }
        if let Some(work) = self.objects.state_mut(owner)
        {
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
        changed
    }

    pub(crate) fn layout_wait_ticks(&self, owner: u64, now: u64, requested: u32) -> u32
    {
        self.objects.state(owner).and_then(|work| work.retry).map_or(requested, |at| {
            requested.min(at.saturating_sub(now).max(1).min(u64::from(u32::MAX)) as u32)
        })
    }

    pub(crate) fn prepare_automatic(&self, root: Root, owner: u64, viewport: Rect,
                                    paint_pending: bool, gate: TrapGate,
                                    resolve: impl FnMut(i16) -> Result<DefinitionEntry, Status>)
                                    -> Result<Prepared, Status>
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
        self.prepare_layout(&call, owner, false, gate, resolve, window)
    }
}
