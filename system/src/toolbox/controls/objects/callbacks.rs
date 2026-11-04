//! Owned Control Manager continuations. Canonical controls retain only weak
//! links; original immutable source/claim custody follows both the request and
//! its canonical running callback outside Desktop.
use alloc::{boxed::Box,
            sync::{Arc, Weak}};
use core::{future::Future,
           pin::Pin,
           task::{Context, Poll, Waker}};

use super::{super::effects::{Author, WaitSlot},
            *};
use crate::{machine::scheduler::accounting::AllocationAuthority,
            toolbox::resources::definitions::{QuiescencePlan,
                                              callbacks::{CompletionTarget, Pending}}};

mod paint;
pub(crate) use paint::{ManagedPaintClaim, ManagedPaintOrigin};

pub(super) struct Claim
{
    return_record: Arc<super::claim_wait::ClaimReturn>
}
impl Claim
{
    pub(super) fn new(record: Arc<super::claim_wait::ClaimReturn>) -> Self
    {
        Self { return_record: record }
    }
}
pub(super) struct ClaimLink
{
    claim:         Weak<Claim>,
    whole_window:  bool,
    return_record: Arc<super::claim_wait::ClaimReturn>
}
impl ClaimLink
{
    pub(super) fn active(&self) -> bool
    {
        self.return_record.active()
    }

    pub(super) fn return_record(&self) -> Option<Arc<super::claim_wait::ClaimReturn>>
    {
        self.return_record.active().then(|| self.return_record.clone())
    }
}
impl Drop for Claim
{
    fn drop(&mut self)
    {
        if !self.return_record.returned()
        {
            return;
        }
        // LAST original claim after semantic readers, including any canonical
        // running callback's custody, really returned outside manager guards.
        // Wake genuine pending cleanup rather than manufacturing a completion.
        super::automatic::claim_returned();
        crate::kernel::window_sources_changed();
        crate::machine::paging::modules::changed();
        crate::toolbox::processes::retirement::resources_released();
    }
}
impl Object
{
    pub(super) fn claimed(&self) -> bool
    {
        self.disposal.as_ref().is_some_and(|cell| cell.active())
        || self.claim.as_ref().is_some_and(|link| link.active())
    }

    pub(super) fn window_claimed(&self) -> bool
    {
        self.claim.as_ref().is_some_and(|link| link.whole_window && link.active())
    }
}
struct Expected
{
    key:       mac_object_table::store::CompletionKey,
    recipient: u64,
    tree:      tree::Node,
    source:    QuiescencePlan
}
impl Expected
{
    fn validate(&self, manager: &Manager, claim: &Arc<Claim>, window: u64) -> Result<(), Status>
    {
        let object = manager.objects.get(self.key.id()).ok_or(NOT_FOUND)?;
        if manager.objects.completion_key(self.key.id()) != Some(self.key)
           || object.window != window
           || object.recipient != self.recipient
           || object.tree != self.tree
           || !self.source.matches(&object.definition)
           || !object.claim.as_ref().is_some_and(|link| link.claim.as_ptr() == Arc::as_ptr(claim))
        {
            return Err(SERVICE_BUSY);
        }
        manager.no_conflicting_claims(self.key.id(), Some(claim))?;
        Ok(())
    }
}
struct WindowClaims
{
    window:      u64,
    root:        Option<u64>,
    owner:       mac_object_table::domain::DomainId,
    _allocation: AllocationAuthority,
    // Real readers leave before the final claim-return publication.
    expected:    Vec<Expected>,
    claim:       Arc<Claim>
}
impl crate::toolbox::resources::definitions::callbacks::CallbackCustody for WindowClaims
{
}
pub(crate) struct WindowLayoutPlan
{
    viewport:  Rect,
    snapshots: Vec<LayoutTask>,
    claims:    Arc<WindowClaims>
}
enum LayoutTask
{
    Tree(grid::LayoutSnapshot),
    Scroll(grid::ScrollSnapshot),
    Hit(super::hit::HitSnapshot, super::super::effects::DetachedCall),
    Tracking(TrackingSnapshot)
}
enum TrackingSnapshot
{
    Split(super::splitter::SplitterSnapshot),
    Probe
    {
        initial:  State,
        point:    Point,
        visible:  Option<Rect>,
        splitter: Option<Result<super::splitter::SplitterSnapshot, Status>>,
        scroll:   Option<Result<grid::ScrollSnapshot, Status>>,
        fallback: Option<RequestSnapshot>
    }
}
impl TrackingSnapshot
{
    async fn calculate(self, author: &Author)
                       -> Result<(Prepared, super::super::effects::DetachedCall), Status>
    {
        let Self::Probe { initial,
                          point,
                          visible,
                          splitter,
                          scroll,
                          fallback } = self
        else
        {
            let Self::Split(snapshot) = self
            else
            {
                return Err(BAD_ABI);
            };
            return snapshot.calculate(author).await;
        };
        // Preserve the native splitter-before-scroll routing. Probe uses the
        // original captured geometry; accepted splitter preparation may then
        // arrange/re-HIT its same new geometry as in the trusted algorithm.
        if let Some(splitter) = splitter
           && !visible.is_some_and(|clip| !clip.contains(point))
           && initial.divider_hit_with(point, author).await?.is_some()
        {
            return splitter?.calculate(author).await;
        }
        if let Some(scroll) = scroll
        {
            return scroll?.calculate(author).await;
        }
        let calculated = fallback.ok_or(BAD_ABI)?.calculate(author).await?;
        let call = calculated.call;
        Ok((Prepared::from_request(calculated)?, call))
    }
}
impl LayoutTask
{
    async fn calculate(self, author: &Author)
                       -> Result<(Prepared, Option<super::super::effects::DetachedCall>), Status>
    {
        match self
        {
            Self::Tracking(snapshot) =>
            {
                let (prepared, call) = snapshot.calculate(author).await?;
                Ok((prepared, Some(call)))
            }
            Self::Hit(snapshot, mut call) =>
            {
                let (child, part) = snapshot.calculate(author).await?;
                call.layout.child = child;
                call.result = i32::from(part);
                Ok((Prepared::empty(), Some(call)))
            }
            Self::Tree(snapshot) => Ok((snapshot.calculate(author).await?, None)),
            Self::Scroll(snapshot) =>
            {
                let (prepared, call) = snapshot.calculate(author).await?;
                Ok((prepared, Some(call)))
            }
        }
    }
}
impl WindowLayoutPlan
{
    pub(crate) fn prepare(self, completion: CompletionTarget, allocation: &AllocationAuthority)
                          -> Result<ControlLayout, (Status, Self)>
    {
        if allocation.owner() != self.claims.owner
        {
            return Err((ACCESS_DENIED, self));
        }
        let target =
            crate::allocator::Target::Owner { account: allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || self.prepare_owned(completion, allocation))
    }

    fn prepare_owned(self, completion: CompletionTarget, allocation: &AllocationAuthority)
                     -> Result<ControlLayout, (Status, Self)>
    {
        let wait = match WaitSlot::new()
        {
            Ok(wait) => wait,
            Err(status) => return Err((status, self))
        };
        let author = Author::staged(crate::machine::traps::gateway,
                                    completion,
                                    wait.clone(),
                                    allocation.clone()).with_custody(self.claims.clone());
        let future_allocation = allocation.clone();
        let future = match pin_calculation(self, move |plan| {
                  async move {
                      let WindowLayoutPlan { claims,
                                             viewport,
                                             snapshots } = plan;
                      let mut prepared = Prepared::empty();
                      let mut output = None;
                      for snapshot in snapshots
                      {
                          let (next, call) = snapshot.calculate(&author).await?;
                          prepared.extend(next)?;
                          if call.is_some()
                          {
                              output = call;
                          }
                      }
                      let measurement = prepared.measurement;
                      let feedback = prepared.feedback;
                      let assigns_geometry = prepared.assigns_geometry();
                      Ok(PreparedWindowLayout { retired_commands: Vec::new(),
                                                allocation: future_allocation,
                                                claims,
                                                viewport,
                                                measurement,
                                                assigns_geometry,
                                                output,
                                                feedback,
                                                prepared: Some(prepared),
                                                published: false })
                  }
              })
        {
            Ok(future) => future,
            Err(refused) => return Err(refused)
        };
        Ok(ControlLayout { future: Some(future),
                           wait,
                           allocation: allocation.clone() })
    }
}

/// Allocation precedes moving the original state into its compiled future.
/// Refused allocation returns that SAME state, without running the factory.
fn pin_calculation<S, F: Future + Send + 'static>(state: S, build: impl FnOnce(S) -> F)
                                                  -> Result<Pin<Box<F>>, (Status, S)>
{
    let allocation = match Box::<F>::try_new_uninit()
    {
        Ok(allocation) => allocation,
        Err(_) => return Err((NO_MEMORY, state))
    };
    Ok(Box::into_pin(Box::write(allocation, build(state))))
}

pub(crate) struct ControlLayout
{
    future:     Option<Pin<Box<dyn Future<Output = Result<PreparedWindowLayout, Status>> + Send>>>,
    wait:       Arc<WaitSlot>,
    allocation: AllocationAuthority
}
pub(crate) enum LayoutProgress
{
    Wait(Arc<Pending>),
    Ready(PreparedWindowLayout)
}
impl ControlLayout
{
    /// The owner calls only on the initial turn or a real broker/availability
    /// edge. A cold Pending is returned without polling calculation again.
    pub(crate) fn advance(&mut self) -> Result<LayoutProgress, Status>
    {
        let target =
            crate::allocator::Target::Owner { account: self.allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || self.advance_owned())
    }

    fn advance_owned(&mut self) -> Result<LayoutProgress, Status>
    {
        if let Some(pending) = self.wait.pending().filter(|pending| !pending.ready())
        {
            return Ok(LayoutProgress::Wait(pending));
        }
        let future = self.future.as_mut().ok_or(BAD_ARGUMENT)?;
        match future.as_mut().poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(result) =>
            {
                let retired = self.future.take();
                drop(retired);
                result.map(LayoutProgress::Ready)
            }
            Poll::Pending => self.wait.pending().map(LayoutProgress::Wait).ok_or(BAD_ABI)
        }
    }
}

pub(crate) struct PreparedWindowLayout
{
    retired_commands: Vec<crate::toolbox::quickdraw::retained::RenderCommand>,
    viewport:         Rect,
    measurement:      mac_abi::controls::reflow::Measurement,
    assigns_geometry: bool,
    output:           Option<super::super::effects::DetachedCall>,
    feedback:         Option<Option<Rect>>,
    // Keep all original source/claim pins even after the scene consumes this.
    prepared:         Option<Prepared>,
    published:        bool,
    allocation:       AllocationAuthority,
    claims:           Arc<WindowClaims>
}
impl PreparedWindowLayout
{
    pub(crate) fn window(&self) -> u64
    {
        self.claims.window
    }

    pub(crate) fn viewport(&self) -> Rect
    {
        self.viewport
    }

    pub(crate) fn changes_geometry(&self) -> bool
    {
        self.assigns_geometry
    }

    pub(crate) fn output(&self) -> Option<ControlCall>
    {
        self.output.map(|call| call.wire())
    }

    pub(crate) fn feedback(&self) -> Option<Option<Rect>>
    {
        self.feedback
    }

    pub(crate) fn measurement(&self) -> mac_abi::controls::reflow::Measurement
    {
        self.measurement
    }

    pub(crate) fn validate(&self, manager: &Manager) -> Result<(), Status>
    {
        if self.published || self.prepared.is_none()
        {
            return Err(BAD_ARGUMENT);
        }
        self.claims.validate(manager)
    }

    /// One validated common publisher. A refusal retains the same calculation;
    /// success consumes its state while this carrier keeps all original pins.
    pub(crate) fn publish<R>(&mut self, manager: &mut Manager,
                             publish: impl FnOnce(&mut Manager, u64, &mut Prepared,
                                                  &mut Vec<crate::toolbox::quickdraw::retained::RenderCommand>)
                                       -> Result<R, Status>)
                             -> Result<R, Status>
    {
        self.validate(manager)?;
        if let Some(id) = self.prepared.as_ref().and_then(Prepared::request_id)
           && !self.claims.expected.iter().any(|expected| expected.key.id() == id)
        {
            return Err(BAD_ABI);
        }
        let target =
            crate::allocator::Target::Owner { account: self.allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || {
            let prepared = self.prepared.as_mut().ok_or(BAD_ARGUMENT)?;
            let result = publish(manager, self.claims.window, prepared, &mut self.retired_commands);
            if result.is_ok()
            {
                self.published = true;
            }
            result
        })
    }
}

impl WindowClaims
{
    fn validate(&self, manager: &Manager) -> Result<(), Status>
    {
        for expected in &self.expected
        {
            expected.validate(manager, &self.claim, self.window)?;
        }
        if let Some(root) = self.root
        {
            // Window0 has no global scene owner. Validate only the original
            // explicit subtree, including every full-generation descendant.
            let mut current = root;
            for expected in &self.expected
            {
                if current != expected.key.id()
                {
                    return Err(SERVICE_BUSY);
                }
                current = manager.next_in(root, current);
            }
            if current != 0
            {
                return Err(SERVICE_BUSY);
            }
        }
        else if manager.objects.iter().filter(|(_, object)| object.window == self.window).count()
                  != self.expected.len()
        {
            return Err(SERVICE_BUSY);
        }
        Ok(())
    }
}

impl Manager
{
    /// Disposal removes a whole subtree. An actual owned operation on that
    /// subtree or any ancestor still owns a claim and snapshot of it, so removal
    /// is refused as busy rather than invalidating the original work.
    pub(super) fn no_claims_on_tree(&self, root: u64) -> Result<(), Status>
    {
        self.no_conflicting_claims(root, None)?;
        let mut descendant = self.objects.get(root).ok_or(NOT_FOUND)?.tree.first;
        while descendant != 0
        {
            if self.objects.get(descendant).ok_or(CORRUPT_DATA)?.claimed()
            {
                return Err(SERVICE_BUSY);
            }
            descendant = self.next_in(root, descendant);
        }
        Ok(())
    }

    /// Public access rejects another active ancestor snapshot. Final commit
    /// permits only its own same affine claim, including whole-window groups.
    pub(super) fn no_conflicting_claims(&self, mut id: u64, allowed: Option<&Arc<Claim>>)
                                        -> Result<(), Status>
    {
        while id != 0
        {
            let object = self.objects.get(id).ok_or(CORRUPT_DATA)?;
            if object.disposal.as_ref().is_some_and(|cell| cell.active())
            {
                return Err(SERVICE_BUSY);
            }
            if let Some(link) = &object.claim
               && link.active()
               && !allowed.is_some_and(|claim| link.claim.as_ptr() == Arc::as_ptr(claim))
            {
                return Err(SERVICE_BUSY);
            }
            id = object.tree.parent;
        }
        Ok(())
    }
}

impl Manager
{
    pub(crate) fn begin_window_layout(&mut self, window: u64, viewport: Rect, paint_pending: bool,
                                      allocation: &AllocationAuthority)
                                      -> Result<WindowLayoutPlan, Status>
    {
        if self.objects
               .iter()
               .any(|(id, object)| {
                   object.window == window && self.objects.owner(id) != Some(allocation.owner())
               })
        {
            return Err(ACCESS_DENIED);
        }
        let target =
            crate::allocator::Target::Owner { account: allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || {
            self.snapshot_claimed_window(window, viewport, paint_pending, allocation)
        })
    }

    fn snapshot_claimed_window(&mut self, window: u64, viewport: Rect, paint_pending: bool,
                               allocation: &AllocationAuthority)
                               -> Result<WindowLayoutPlan, Status>
    {
        Self::check_viewport(viewport)?;
        let mut snapshots = Vec::new();
        let count = self.objects
                        .iter()
                        .filter(|(_, object)| object.window == window && object.tree.parent == 0)
                        .count();
        snapshots.try_reserve_exact(count).map_err(|_| NO_MEMORY)?;
        for (id, object) in
            self.objects
                .iter()
                .filter(|(_, object)| object.window == window && object.tree.parent == 0)
        {
            if object.claimed()
            {
                return Err(SERVICE_BUSY);
            }
            if let Some(snapshot) =
                self.window_root_snapshot(id, object, viewport, paint_pending, None)?
            {
                snapshots.push(LayoutTask::Tree(snapshot));
            }
        }
        // No source/claim lease exists on any earlier snapshot refusal.
        let claims = self.claim_window(window, allocation)?;
        Ok(WindowLayoutPlan { claims,
                              viewport,
                              snapshots })
    }

    fn claim_window(&mut self, window: u64, allocation: &AllocationAuthority)
                    -> Result<Arc<WindowClaims>, Status>
    {
        let count = self.objects.iter().filter(|(_, object)| object.window == window).count();
        let mut keys = Vec::new();
        keys.try_reserve_exact(count).map_err(|_| NO_MEMORY)?;
        for (id, _) in self.objects.iter().filter(|(_, object)| object.window == window)
        {
            keys.push(self.objects.completion_key(id).ok_or(NOT_FOUND)?);
        }
        self.claim_layout_keys(window, None, allocation, keys)
    }

    fn claim_layout_subtree(&mut self, root: u64, allocation: &AllocationAuthority)
                            -> Result<Arc<WindowClaims>, Status>
    {
        self.no_claims_on_tree(root)?;
        let window = self.objects.get(root).ok_or(NOT_FOUND)?.window;
        let ids = self.subtree(root)?;
        let mut keys = Vec::new();
        keys.try_reserve_exact(ids.len()).map_err(|_| NO_MEMORY)?;
        for id in ids
        {
            keys.push(self.objects.completion_key(id).ok_or(NOT_FOUND)?);
        }
        self.claim_layout_keys(window, Some(root), allocation, keys)
    }

    fn claim_layout_keys(&mut self, window: u64, root: Option<u64>,
                         allocation: &AllocationAuthority,
                         keys: Vec<mac_object_table::store::CompletionKey>)
                         -> Result<Arc<WindowClaims>, Status>
    {
        let mut expected = Vec::new();
        expected.try_reserve_exact(keys.len()).map_err(|_| NO_MEMORY)?;
        for key in &keys
        {
            let object = self.objects.get(key.id()).ok_or(NOT_FOUND)?;
            if key.owner() != allocation.owner() || object.window != window
            {
                return Err(ACCESS_DENIED);
            }
            if object.claimed()
            {
                return Err(SERVICE_BUSY);
            }
        }
        let record = super::claim_wait::ClaimReturn::prepare(self.claim_presence.clone())?;
        let claim = Arc::try_new(Claim::new(record.clone())).map_err(|_| NO_MEMORY)?;
        // SAME exclusive Desktop turn; no mutation/author call after key
        // preflight. Only original canonical payloads can supply these readers.
        for key in keys
        {
            if let Some(object) = self.objects.get(key.id())
            {
                expected.push(Expected { key,
                                         recipient: object.recipient,
                                         tree: object.tree,
                                         source: object.definition.quiescence_plan() });
            }
        }
        let claims = Arc::try_new(WindowClaims { window,
                                                 root,
                                                 owner: allocation.owner(),
                                                 _allocation: allocation.clone(),
                                                 expected,
                                                 claim }).map_err(|_| NO_MEMORY)?;
        // All allocations and source readers are owned before any canonical
        // link changes. A failed dormant admission never publishes a return.
        record.bind()?;
        for entry in &claims.expected
        {
            if let Some(object) = self.objects.get_mut(entry.key.id())
            {
                object.claim = Some(ClaimLink { claim:         Arc::downgrade(&claims.claim),
                                                whole_window:  root.is_none(),
                                                return_record: record.clone() });
            }
        }
        Ok(claims)
    }
}

/// Same pure request calculation used by trusted immediate and owned user work.
pub(super) struct RequestSnapshot
{
    pub(super) call:           super::super::effects::DetachedCall,
    pub(super) next:           State,
    pub(super) window:         u64,
    pub(super) tree:           tree::Node,
    pub(super) entry:          DefinitionRef,
    pub(super) inherited_clip: Option<Rect>,
    pub(super) layout_status:  Status,
    pub(super) admitted_title: Option<Arc<super::super::snapshot::Title>>
}
pub(super) struct RequestCalculation
{
    pub(super) call:     super::super::effects::DetachedCall,
    pub(super) next:     State,
    pub(super) previous: Spec,
    pub(super) drawing:  Option<super::super::Drawing>
}
impl RequestSnapshot
{
    pub(super) async fn calculate(self, author: &Author) -> Result<RequestCalculation, Status>
    {
        let mut next = self.next;
        let mut call = self.call;
        let entry = self.entry;
        let operation = call.operation;
        let inherited_clip = self.inherited_clip;
        let layout_status = self.layout_status;
        let admitted_title = self.admitted_title;
        let o = &mut next;
        let previous = o.spec;
        let previous_visible =
            o.layout
             .as_ref()
             .and_then(Layout::clip)
             .map_or(previous.bounds(), |clip| super::super::intersection(previous.bounds(), clip));
        let mut action = false;
        let mut handled = false;
        let mut part = 0;
        let mut visibility_changed = false;
        match operation
        {
            CREATE_STANDARD | CREATE_SCROLLBAR | INFO_OBJECT | DRAW_OBJECT => (),
            REFLOW_OBJECT =>
            {
                let assigned_hidden = call.layout.message == mac_abi::controls::reflow::ASSIGN_RECT
                                      && o.layout.as_ref().is_some_and(|l| l.hidden);
                if assigned_hidden && let Some(l) = &mut o.layout
                {
                    l.hidden = false;
                }
                visibility_changed =
                    o.reflow_with(&mut call.layout, entry, author).await? || assigned_hidden;
            }
            SET_TABLE_LINES_OBJECT =>
            {
                if call.standard.flags & !TABLE_LINE_FLAGS != 0
                {
                    return Err(BAD_ARGUMENT);
                }
                let Spec::Standard { state,
                                     procedure: TABLE_PROCEDURE } = &mut o.spec
                else
                {
                    return Err(NOT_SUPPORTED);
                };
                state.flags = call.standard.flags;
            }
            CONFIGURE_OBJECT =>
            {
                // New title backing was admitted before any state/capture mutation.
                o.title = admitted_title;
                let next = Spec::from_call(&call.wire(), o.spec.scrollbar().is_some());
                let cancel = o.spec.cancels_capture(next, o.tracking);
                let highlight = if cancel { 0 } else { o.spec.highlight() };
                o.spec = next;
                if previous.bounds() != o.spec.bounds()
                   && let Some(l) = &mut o.layout
                {
                    l.hidden = false;
                }
                if cancel
                {
                    o.tracking = None;
                    o.scroll_axis = None;
                    o.splitter = None;
                    o.scroll_highlight = 0;
                }
                o.spec.set_highlight(highlight);
                if previous != o.spec
                   && let Some(layout) = &mut o.layout
                {
                    layout.measured = None;
                    layout.intrinsic = None;
                    layout.arrangement_pending = true;
                }
            }
            BEGIN_TRACKING =>
            {
                if o.tracking.is_some()
                {
                    return Err(SERVICE_BUSY);
                }
                part = if inherited_clip.is_some_and(|clip| !clip.contains(call.point))
                {
                    0
                }
                else
                {
                    o.hit_with(call.point, entry, author).await?
                };
                if matches!(part,
                            BUTTON_PART
                            | CHECKBOX_PART
                            | SCROLL_UP
                            | SCROLL_DOWN
                            | SCROLL_PAGE_UP
                            | SCROLL_PAGE_DOWN
                            | SCROLL_THUMB)
                {
                    handled = true;
                    o.tracking =
                        Some(Tracking::new(part,
                                           call.point,
                                           o.spec.scrollbar().map_or(0, |state| state.value),
                                           call.ticks));
                    o.highlight(part);
                    if o.spec.scrollbar().is_some() && part != SCROLL_THUMB
                    {
                        action = o.spec.step(part);
                    }
                }
            }
            POLL_TRACKING =>
            {
                if let Some(tracking) = o.tracking
                {
                    handled = true;
                    part = tracking.part;
                    let hit_state: &State = o;
                    let transition = tracking.poll_with(call.point,
                                                call.flags & BUTTON_DOWN != 0,
                                                call.ticks,
                                                o.spec.scrollbar(),
                                                u32::from(o.layout.is_some()),
                                                entry,
                                                author,
                                                |point| {
                                                    async move {
                                                        if inherited_clip.is_some_and(|clip| {
                                                                             !clip.contains(point)
                                                                         })
                                                        {
                                                            Ok(0)
                                                        }
                                                        else
                                                        {
                                                            hit_state.hit_with(point, entry, author)
                                                                     .await
                                                        }
                                                    }
                                                })
                                     .await?;
                    action = match transition.action
                    {
                        Action::None => false,
                        Action::Activate => true,
                        Action::Step(part) => o.spec.step(part),
                        Action::Value(value) => o.spec.set_value(value)
                    };
                    o.highlight(transition.highlight);
                    o.tracking = transition.tracking;
                }
            }
            CANCEL_TRACKING =>
            {
                handled = o.tracking.take().is_some();
                o.highlight(0);
            }
            _ => return Err(NOT_SUPPORTED)
        }
        let changed = visibility_changed || !previous.appearance_matches(o.spec);
        call.window = self.window;
        call.relation = self.tree.relation();
        call = {
            let mut wire = call.wire();
            o.spec.write(&mut wire);
            super::super::effects::DetachedCall::capture(&wire)?
        };
        call.reflow = u32::from(o.layout.is_some());
        call.layout.anchors = o.layout.as_ref().map_or(0, |l| l.anchors.into());
        call.layout.window_border =
            o.layout.as_ref().map_or(Default::default(), |l| l.window_border.into());
        call.layout.invalidation = o.layout.as_ref().map_or(0, Layout::pending);
        call.layout.status = layout_status;
        call.result = i32::from(part);
        call.flags = (u32::from(o.tracking.is_some()) * TRACKING)
                     | (u32::from(handled) * HANDLED)
                     | (u32::from(action) * ACTION)
                     | (u32::from(changed) * CHANGED);
        let drawing = if changed || operation == DRAW_OBJECT
        {
            let state = {
                let mut wire = o.spec.draw();
                wire.reflow = u32::from(o.layout.is_some());
                super::super::effects::DetachedCall::capture(&wire)?
            };
            let (_, drawing) = author.prepare(state, entry, o.title.as_ref()).await?;
            let mut drawing = drawing.ok_or(BAD_ARGUMENT)?;
            drawing.id = call.id;
            drawing.grid = o.layout.as_ref().and_then(|l| l.outlines.clone());
            Manager::scroll_drawing(o, &mut drawing)?;
            if changed
               && operation == CONFIGURE_OBJECT
               && let (Spec::Standard { state: before,
                                        procedure: old },
                       Spec::Standard { state: after,
                                        procedure: new }) = (previous, o.spec)
               && before.bounds == after.bounds
               && old == new
            {
                drawing.change_from_with(before, author).await?;
            }
            drawing.set_visible(tree::clip(o.layout.as_ref().and_then(Layout::clip),
                                           inherited_clip));
            if visibility_changed
            {
                drawing.damage = [previous_visible, drawing.visible_bounds()];
            }
            Some(drawing)
        }
        else
        {
            None
        };
        Ok(RequestCalculation { call,
                                next,
                                previous,
                                drawing })
    }
}

pub(super) struct Configuration
{
    pub(super) call:           super::super::effects::DetachedCall,
    pub(super) scrollbar:      bool,
    pub(super) validate:       bool,
    pub(super) previous_title: Option<Arc<super::super::snapshot::Title>>
}
pub(super) struct Configured
{
    pub(super) call:      super::super::effects::DetachedCall,
    pub(super) scrollbar: bool,
    pub(super) title:     Option<Arc<super::super::snapshot::Title>>
}
impl Configuration
{
    pub(super) async fn prepare(self, reference: DefinitionRef, author: &Author)
                                -> Result<Configured, Status>
    {
        let call =
            if self.validate { author.validate(self.call, reference).await? } else { self.call };
        let title = if !self.scrollbar && call.procedure != PROGRESS_BAR_PROCEDURE
        {
            Some(super::super::snapshot::Title::admit(&call.standard,
                                                      self.previous_title.as_ref())?)
        }
        else
        {
            None
        };
        Ok(Configured { call,
                        scrollbar: self.scrollbar,
                        title })
    }
}

pub(super) struct CommitEffects
{
    pub(super) parent:               u64,
    pub(super) changed_requirements: bool,
    pub(super) changed_geometry:     bool,
    pub(super) newly_pending:        bool,
    pub(super) configured:           bool,
    pub(super) released_capture:     bool,
    pub(super) retired_binding:      Option<BoundDefinition>
}
impl CommitEffects
{
    pub(super) fn apply(object: &mut Object, next: &mut State, previous: Spec, operation: u32,
                        replacement: Option<BoundDefinition>)
                        -> Self
    {
        let parent = object.tree.parent;
        let changed_requirements =
            object.state.layout.as_ref().map(|v| {
                                            (v.sizing.requirements(),
                                             v.measured.is_none(),
                                             v.anchors)
                                        })
            != next.layout
                   .as_ref()
                   .map(|v| (v.sizing.requirements(), v.measured.is_none(), v.anchors));
        let changed_geometry = object.state.spec.bounds() != next.spec.bounds();
        let newly_pending = next.layout.as_ref().is_some_and(|l| l.arrangement_pending)
                            && object.state.layout.as_ref().is_some_and(|l| !l.arrangement_pending);
        if changed_geometry && let Some(l) = &mut next.layout
        {
            l.window_extent = None;
        }
        let released_capture = object.state.tracking.is_some() && next.tracking.is_none();
        let configured = operation == CONFIGURE_OBJECT && previous != next.spec;
        let retired_binding =
            replacement.map(|next| core::mem::replace(&mut object.definition, next));
        core::mem::swap(&mut object.state, next);
        Self { parent,
               changed_requirements,
               changed_geometry,
               newly_pending,
               configured,
               released_capture,
               retired_binding }
    }
}

struct ControlClaims
{
    window:         u64,
    recipient:      OwnerRef,
    _allocation:    AllocationAuthority,
    expected:       Expected,
    inherited_clip: Option<Rect>,
    tree_geometry:  bool,
    claim:          Arc<Claim>
}
impl crate::toolbox::resources::definitions::callbacks::CallbackCustody for ControlClaims
{
}
impl ControlClaims
{
    fn validate(&self, manager: &Manager) -> Result<(), Status>
    {
        if !self.recipient.accepting()
        {
            return Err(ACCESS_DENIED);
        }
        self.expected.validate(manager, &self.claim, self.window)?;
        if manager.inherited_clip(self.expected.key.id()) != self.inherited_clip
        {
            return Err(SERVICE_BUSY);
        }
        Ok(())
    }
}

pub(crate) struct ControlOperationPlan
{
    snapshot:       RequestSnapshot,
    configuration:  Option<Configuration>,
    replacement:    Option<DefinitionRef>,
    cleanup_notice: Arc<crate::toolbox::resources::definitions::callbacks::Notice>,
    claims:         Arc<ControlClaims>
}
impl ControlOperationPlan
{
    pub(crate) fn prepare(self, completion: CompletionTarget, allocation: &AllocationAuthority)
                          -> Result<ControlWork, (Status, Self)>
    {
        if allocation.owner() != self.claims.expected.key.owner()
        {
            return Err((ACCESS_DENIED, self));
        }
        let target =
            crate::allocator::Target::Owner { account: allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || self.prepare_owned(completion, allocation))
    }

    fn prepare_owned(self, completion: CompletionTarget, allocation: &AllocationAuthority)
                     -> Result<ControlWork, (Status, Self)>
    {
        let wait = match WaitSlot::new()
        {
            Ok(wait) => wait,
            Err(status) => return Err((status, self))
        };
        let author = Author::staged(crate::machine::traps::gateway,
                                    completion,
                                    wait.clone(),
                                    allocation.clone()).with_custody(self.claims.clone());
        let future_allocation = allocation.clone();
        let future = match pin_calculation(self, move |plan| {
                  async move {
                      let ControlOperationPlan { claims,
                                                 mut snapshot,
                                                 configuration,
                                                 replacement,
                                                 cleanup_notice } = plan;
                      let replacement =
                          replacement.map(crate::toolbox::resources::definitions::bind)
                                     .transpose()?;
                      let cleanup_change = replacement.is_some();
                      let cleanup = if let Some(binding) =
                          replacement.as_ref().filter(|binding| binding.is_process_source())
                      {
                          let wire = snapshot.call.wire();
                          let request =
                              super::super::user_disposal_request(&wire, binding.resource_id());
                          Some(crate::toolbox::resources::definitions::callbacks::PreparedControlDisposal::prepare(
                              binding.reference(),&request,CompletionTarget::System(cleanup_notice),&future_allocation)?)
                      }
                      else
                      {
                          None
                      };
                      if let Some(configuration) = configuration
                      {
                          snapshot.admitted_title =
                              configuration.prepare(snapshot.entry, &author).await?.title;
                      }
                      let calculated = snapshot.calculate(&author).await?;
                      Ok(PreparedControlOperation { cleanup_change,
                                                    cleanup,
                                                    retired_cleanup: None,
                                                    retired_drawing: None,
                                                    retired_binding: None,
                                                    calculated,
                                                    replacement,
                                                    claims,
                                                    published: false })
                  }
              })
        {
            Ok(future) => future,
            Err(refused) => return Err(refused)
        };
        Ok(ControlWork { future: Some(future),
                         wait,
                         allocation: allocation.clone() })
    }
}

pub(crate) struct ControlWork
{
    future: Option<Pin<Box<dyn Future<Output = Result<PreparedControlOperation, Status>> + Send>>>,
    wait:       Arc<WaitSlot>,
    allocation: AllocationAuthority
}
pub(crate) enum ControlProgress
{
    Wait(Arc<Pending>),
    Ready(PreparedControlOperation)
}
impl ControlWork
{
    pub(crate) fn advance(&mut self) -> Result<ControlProgress, Status>
    {
        let target =
            crate::allocator::Target::Owner { account: self.allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || self.advance_owned())
    }

    fn advance_owned(&mut self) -> Result<ControlProgress, Status>
    {
        if let Some(pending) = self.wait.pending().filter(|pending| !pending.ready())
        {
            return Ok(ControlProgress::Wait(pending));
        }
        let future = self.future.as_mut().ok_or(BAD_ARGUMENT)?;
        match future.as_mut().poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(result) =>
            {
                let retired = self.future.take();
                drop(retired);
                result.map(ControlProgress::Ready)
            }
            Poll::Pending => self.wait.pending().map(ControlProgress::Wait).ok_or(BAD_ABI)
        }
    }
}

pub(crate) struct PreparedControlOperation
{
    retired_cleanup:
        Option<crate::toolbox::resources::definitions::callbacks::PreparedControlDisposal>,
    cleanup: Option<crate::toolbox::resources::definitions::callbacks::PreparedControlDisposal>,
    cleanup_change:  bool,
    // Retired render/binding/state readers end before releasing the real claim.
    retired_drawing: Option<crate::toolbox::quickdraw::retained::RenderCommand>,
    retired_binding: Option<BoundDefinition>,
    calculated:      RequestCalculation,
    replacement:     Option<BoundDefinition>,
    claims:          Arc<ControlClaims>,
    published:       bool
}
impl PreparedControlOperation
{
    pub(crate) fn window(&self) -> u64
    {
        self.claims.window
    }

    pub(crate) fn changes_tree_geometry(&self) -> bool
    {
        self.claims.tree_geometry
    }

    pub(crate) fn output(&self) -> ControlCall
    {
        self.calculated.call.wire()
    }

    pub(crate) fn validate(&self, manager: &Manager) -> Result<(), Status>
    {
        if self.published
        {
            return Err(BAD_ARGUMENT);
        }
        self.claims.validate(manager)
    }

    /// Admission may inspect/take only the drawing, never the state or claim.
    /// A refusal must retain SAME Option; success commits once without fallible
    /// work and retains every retired payload for outer guard-free destruction.
    pub(crate) fn publish<R>(&mut self, manager: &mut Manager,
                             publish: impl FnOnce(&mut Option<super::super::Drawing>)
            -> Result<(R, Option<crate::toolbox::quickdraw::retained::RenderCommand>), Status>)
                             -> Result<Option<R>, Status>
    {
        self.validate(manager)?;
        if let Some(drawing) = &mut self.calculated.drawing
        {
            manager.seal_managed_paint_origin(self.claims.window, drawing)?;
        }
        let cleanup_cell = if self.cleanup_change
        {
            let cell = manager.objects
                              .get(self.claims.expected.key.id())
                              .ok_or(NOT_FOUND)?
                              .disposal
                              .as_ref()
                              .ok_or(BAD_ABI)?
                              .clone();
            cell.validate_stock_replacement()?;
            Some(cell)
        }
        else
        {
            None
        };
        let object = manager.objects.get_mut(self.claims.expected.key.id()).ok_or(NOT_FOUND)?;
        let result = if self.calculated.drawing.is_some()
        {
            let (result, retired) = publish(&mut self.calculated.drawing)?;
            self.retired_drawing = retired;
            Some(result)
        }
        else
        {
            None
        };
        let wire = self.calculated.call.wire();
        let effects = CommitEffects::apply(object,
                                           &mut self.calculated.next,
                                           self.calculated.previous,
                                           wire.operation,
                                           self.replacement.take());
        self.retired_binding = manager.apply_request_effects(&wire, effects);
        if let Some(cell) = cleanup_cell
        {
            self.retired_cleanup = cell.replace_stock(self.cleanup.take());
        }
        self.published = true;
        Ok(result)
    }
}

impl Manager
{
    fn claim_control(&mut self, id: u64, recipient: &OwnerRef, allocation: &AllocationAuthority,
                     tree_geometry: bool)
                     -> Result<Arc<ControlClaims>, Status>
    {
        self.get(id, recipient.owner().incarnation())?;
        // One owned operation per control: a second would replace the first
        // claim, and a DRAW must never follow an admitted author DISPOSE.
        if self.objects.get(id).ok_or(NOT_FOUND)?.claimed()
        {
            return Err(SERVICE_BUSY);
        }
        // A parent snapshot includes layout caches invalidated by descendants.
        // Admit neither side of an overlapping ancestor/descendant operation.
        let mut descendant = self.objects.get(id).ok_or(NOT_FOUND)?.tree.first;
        while descendant != 0
        {
            if self.objects.get(descendant).ok_or(CORRUPT_DATA)?.claimed()
            {
                return Err(SERVICE_BUSY);
            }
            descendant = self.next_in(id, descendant);
        }
        let key = self.objects.completion_key(id).ok_or(NOT_FOUND)?;
        if key.owner() != allocation.owner() || recipient.owner() != allocation.owner()
        {
            return Err(ACCESS_DENIED);
        }
        let inherited_clip = self.inherited_clip(id);
        let object = self.objects.get_mut(id).ok_or(NOT_FOUND)?;
        let record = super::claim_wait::ClaimReturn::prepare(self.claim_presence.clone())?;
        let claim = Arc::try_new(Claim::new(record.clone())).map_err(|_| NO_MEMORY)?;
        // Source and shared keeper preparation precede all weak-claim changes. The
        // unique mutable canonical payload stays held through this publication.
        let expected = Expected { key,
                                  recipient: object.recipient,
                                  tree: object.tree,
                                  source: object.definition.quiescence_plan() };
        let claims = Arc::try_new(ControlClaims { window: object.window,
                                                  recipient: recipient.clone(),
                                                  _allocation: allocation.clone(),
                                                  expected,
                                                  inherited_clip,
                                                  tree_geometry,
                                                  claim }).map_err(|_| NO_MEMORY)?;
        record.bind()?;
        object.claim = Some(ClaimLink { claim:         Arc::downgrade(&claims.claim),
                                        whole_window:  false,
                                        return_record: record });
        Ok(claims)
    }

    /// Existing leaf operations only; tree/scroll/splitter batches have their
    /// own shared layout calculation and require their whole real footprint.
    pub(crate) fn plan_control_operation(&mut self, recipient: &OwnerRef, storage: &OwnerRef,
                                         input: &ControlCall, entry: DefinitionRef,
                                         allocation: &AllocationAuthority)
                                         -> Result<Option<ControlOperationPlan>, Status>
    {
        if recipient.owner() != storage.owner()
           || recipient.owner() != allocation.owner()
           || !recipient.accepting()
        {
            return Err(ACCESS_DENIED);
        }
        let target =
            crate::allocator::Target::Owner { account: allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || {
            self.snapshot_control_operation(recipient, storage, input, entry, allocation)
        })
    }

    pub(crate) fn classify_control_operation(&self, input: &ControlCall, owner: u64)
                                             -> Result<Option<ControlCall>, Status>
    {
        let mut call = *input;
        self.prepare_static_text(&mut call, owner)?;
        if call.reflow != 0 || !call.surface.is_null() || call.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        if !matches!(call.operation,
                     CONFIGURE_OBJECT
                     | REFLOW_OBJECT
                     | DRAW_OBJECT
                     | SET_TABLE_LINES_OBJECT
                     | BEGIN_TRACKING
                     | POLL_TRACKING)
        {
            return Ok(None);
        }
        let object = self.snapshot(call.id, owner)?;
        if call.operation == REFLOW_OBJECT
        {
            use mac_abi::controls::reflow as wire;
            if !matches!(call.layout.message,
                         wire::ASK_SIZE
                         | wire::ASSIGN_RECT
                         | wire::INVALIDATE_LAYOUT
                         | wire::SET_VISIBLE_RECT
                         | wire::SET_ANCHORS
                         | wire::SET_MINIMUM
                         | wire::CLEAR_MINIMUM
                         | wire::SET_PREFERRED
                         | wire::CLEAR_PREFERRED
                         | wire::SET_SIZE_PREFERENCES)
            {
                return Ok(None);
            }
            if (matches!(call.layout.message, wire::ASK_SIZE | wire::ASSIGN_RECT)
                && matches!(object.state.spec,
                            Spec::Standard { procedure: TABLE_PROCEDURE,
                                             .. }))
               || (call.layout.message == wire::SET_ANCHORS && object.tree.parent == 0)
            {
                return Ok(None);
            }
        }
        if object.state.splitter.is_some()
           || object.state.layout.as_ref().is_some_and(|layout| layout.scroll.is_some())
        {
            return Ok(None);
        }
        if matches!(call.operation, BEGIN_TRACKING | POLL_TRACKING)
           && (object.state.layout.as_ref().is_some_and(|layout| layout.scroll.is_some())
               || object.state.splitter.is_some()
               || matches!(object.state.spec,
                           Spec::Standard { procedure: SPLITTER_PROCEDURE,
                                            .. })
               || object.state
                        .layout
                        .as_ref()
                        .and_then(|layout| layout.outlines.as_ref())
                        .is_some_and(|grid| grid.dividers.is_some()))
        {
            return Ok(None);
        }

        // Query/no-change operations remain synchronous without heap admission.
        if call.operation == POLL_TRACKING && object.state.tracking.is_none()
        {
            return Ok(None);
        }
        if call.operation == CONFIGURE_OBJECT && !self.configuration(&call, owner)?.validate
        {
            return Ok(None);
        }
        if call.operation == BEGIN_TRACKING
           && (self.inherited_clip(call.id).is_some_and(|clip| !clip.contains(call.point))
               || object.state
                        .layout
                        .as_ref()
                        .and_then(Layout::clip)
                        .is_some_and(|clip| !clip.contains(call.point)))
        {
            return Ok(None);
        }
        if call.operation == REFLOW_OBJECT
        {
            use mac_abi::controls::reflow as wire;
            if matches!(call.layout.message,
                        wire::SET_MINIMUM
                        | wire::CLEAR_MINIMUM
                        | wire::SET_PREFERRED
                        | wire::CLEAR_PREFERRED
                        | wire::SET_SIZE_PREFERENCES
                        | wire::SET_ANCHORS)
            {
                return Ok(None);
            }
            if let Some(layout) = &object.state.layout
            {
                if call.layout.message == wire::ASSIGN_RECT
                   && call.layout.rect == object.state.spec.bounds()
                   && !layout.hidden
                {
                    return Ok(None);
                }
                if call.layout.message == wire::SET_VISIBLE_RECT
                   && layout.visible == Some(call.layout.rect)
                {
                    return Ok(None);
                }
                if call.layout.message == wire::ASK_SIZE
                {
                    let constraints: layout::Constraints =
                        call.layout
                            .constraints
                            .try_into()
                            .map_err(|error: layout::Error| error.status())?;
                    if (constraints == Default::default() && layout.intrinsic.is_some())
                       || layout.measured.is_some_and(|(key, _)| key == constraints)
                    {
                        return Ok(None);
                    }
                }
            }
        }
        Ok(Some(call))
    }

    fn snapshot_control_operation(&mut self, recipient: &OwnerRef, storage: &OwnerRef,
                                  input: &ControlCall, entry: DefinitionRef,
                                  allocation: &AllocationAuthority)
                                  -> Result<Option<ControlOperationPlan>, Status>
    {
        let owner = recipient.owner().incarnation();
        let Some(call) = self.classify_control_operation(input, owner)?
        else
        {
            return Ok(None);
        };
        let (_, bound) = self.preflight_definition(recipient, storage, &call)?;
        let entry = bound.unwrap_or(entry);
        let object = self.get(call.id, owner)?;
        if !crate::toolbox::resources::definitions::is_user(entry)?
           && !(call.operation == CONFIGURE_OBJECT && object.definition.is_process_source())
        {
            return Ok(None);
        }
        let configuration = if call.operation == CONFIGURE_OBJECT
        {
            Some(self.configuration(&call, owner)?)
        }
        else
        {
            None
        };
        let snapshot = RequestSnapshot { call:
                                             super::super::effects::DetachedCall::capture(&call)?,
                                         next: object.state.clone(),
                                         window: object.window,
                                         tree: object.tree,
                                         entry,
                                         inherited_clip: self.inherited_clip(call.id),
                                         layout_status: self.layout_status(call.id),
                                         admitted_title: None };
        let replacement = (call.operation == CONFIGURE_OBJECT && bound.is_none()).then_some(entry);
        let tree_geometry = self.changes_tree_geometry(&call, owner)?;
        let claims = self.claim_control(call.id, recipient, allocation, tree_geometry)?;
        Ok(Some(ControlOperationPlan { claims,
                                       snapshot,
                                       configuration,
                                       replacement,
                                       cleanup_notice: self.disposal_notice.clone() }))
    }

    pub(crate) fn process_control_source(&self, id: u64, owner: u64) -> Result<bool, Status>
    {
        Ok(self.snapshot(id, owner)?.definition.is_process_source())
    }

    /// Only a root whose subtree contains an app-owned definition elects the
    /// owned disposal; trusted definitions keep the synchronous path.
    pub(crate) fn disposal_needs_author(&self, call: &ControlCall, owner: u64)
                                        -> Result<bool, Status>
    {
        if call.operation != DISPOSE_OBJECT
        {
            return Ok(false);
        }
        if !call.surface.is_null() || call.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        let root = self.get(call.id, owner)?;
        if root.tree.parent != 0
        {
            return Ok(false);
        }
        for id in self.subtree(call.id)?
        {
            let object = self.get(id, owner)?;
            if object.definition.is_process_source()
               && crate::toolbox::resources::definitions::is_user(object.definition.reference())?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

impl Manager
{
    /// Native table sizing and explicit root-anchor/border edits reuse the
    /// SAME owned calculation and retained publication as Window geometry.
    pub(crate) fn begin_control_layout(&mut self, call: &ControlCall, owner: u64, viewport: Rect,
                                       paint_pending: bool, allocation: &AllocationAuthority)
                                       -> Result<WindowLayoutPlan, Status>
    {
        let window = self.get(call.id, owner)?.window;
        if window != 0
           && self.objects
                  .iter()
                  .any(|(id, object)| {
                      object.window == window && self.objects.owner(id) != Some(allocation.owner())
                  })
        {
            return Err(ACCESS_DENIED);
        }
        let target =
            crate::allocator::Target::Owner { account: allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || {
            let object = self.get(call.id, owner)?;
            let window_edit = matches!(call.layout.message,
                                       mac_abi::controls::reflow::SET_ANCHORS
                                       | mac_abi::controls::reflow::SET_WINDOW_BORDER);
            let scroll = call.operation == REFLOW_OBJECT
                         && matches!(call.layout.message,
                                     mac_abi::controls::reflow::SET_SCROLLING
                                     | mac_abi::controls::reflow::SCROLL_TO
                                     | mac_abi::controls::reflow::REVEAL_CHILD);
            let snapshot = if matches!(call.operation, BEGIN_TRACKING | POLL_TRACKING)
            {
                Some(LayoutTask::Tracking(self.snapshot_tracking(call, owner, paint_pending)?))
            }
            else if call.operation == HIT_CHILDREN_OBJECT
            {
                Some(LayoutTask::Hit(self.snapshot_hit_children(call.id, owner, call.point)?,
                                     super::super::effects::DetachedCall::capture(call)?))
            }
            else if scroll
            {
                let mut call = *call;
                Some(LayoutTask::Scroll(self.snapshot_scroll(&mut call, owner, paint_pending, |procedure| crate::toolbox::runtime::Toolbox::control_definition(procedure))?))
            }
            else if window_edit
            {
                self.window_root_snapshot(call.id,
                                          object,
                                          viewport,
                                          paint_pending,
                                          Some((call, owner)))?
                    .map(LayoutTask::Tree)
            }
            else
            {
                if !matches!(call.layout.message,
                             mac_abi::controls::reflow::ASK_SIZE
                             | mac_abi::controls::reflow::ASSIGN_RECT)
                   || !self.is_table(call.id, owner)?
                {
                    return Err(BAD_ARGUMENT);
                }
                Some(LayoutTask::Tree(self.snapshot_layout_change(call, owner, paint_pending,
                                                                  None, None, None)?))
            };
            let mut snapshots = Vec::new();
            if let Some(snapshot) = snapshot
            {
                snapshots.try_reserve_exact(1).map_err(|_| NO_MEMORY)?;
                snapshots.push(snapshot);
            }
            let claims = if window == 0
            {
                self.claim_layout_subtree(call.id, allocation)?
            }
            else
            {
                self.claim_window(window, allocation)?
            };
            Ok(WindowLayoutPlan { claims,
                                  viewport,
                                  snapshots })
        })
    }
}

impl Manager
{
    pub(crate) fn begin_automatic_layout(&mut self, root: super::automatic::Root,
                                         owner: &OwnerRef, viewport: Rect, paint_pending: bool,
                                         allocation: &AllocationAuthority)
                                         -> Result<WindowLayoutPlan, Status>
    {
        if owner.owner() != allocation.owner() || !owner.accepting()
        {
            return Err(ACCESS_DENIED);
        }
        let target =
            crate::allocator::Target::Owner { account: allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || {
            if self.objects
                   .iter()
                   .any(|(id, object)| {
                       object.window == root.window && self.objects.owner(id) != Some(owner.owner())
                   })
            {
                return Err(ACCESS_DENIED);
            }
            let snapshot = self.snapshot_automatic(root,
                                                   owner.owner().incarnation(),
                                                   viewport,
                                                   paint_pending)?;
            let mut snapshots = Vec::new();
            snapshots.try_reserve_exact(1).map_err(|_| NO_MEMORY)?;
            snapshots.push(LayoutTask::Tree(snapshot));
            let claims = self.claim_window(root.window, allocation)?;
            Ok(WindowLayoutPlan { claims,
                                  viewport,
                                  snapshots })
        })
    }
}

impl Manager
{
    pub(crate) fn tracking_layout_operation(&self, call: &ControlCall, owner: u64)
                                            -> Result<bool, Status>
    {
        if !matches!(call.operation, BEGIN_TRACKING | POLL_TRACKING)
        {
            return Ok(false);
        }
        if !call.surface.is_null() || call.reflow != 0 || call.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        let object = self.snapshot(call.id, owner)?;
        if call.window != 0 && call.window != object.window
        {
            return Err(BAD_ARGUMENT);
        }
        if call.operation == POLL_TRACKING
           && object.state.tracking.is_none()
           && object.state.splitter.is_none()
        {
            return Ok(false);
        }
        if self.visible_clip(call.id).is_some_and(|clip| !clip.contains(call.point))
           && call.operation == BEGIN_TRACKING
        {
            return Ok(false);
        }
        let compound =
            object.state.splitter.is_some()
            || matches!(object.state.spec,
                        Spec::Standard { procedure: SPLITTER_PROCEDURE,
                                         .. })
            || object.state.layout.as_ref().is_some_and(|layout| {
                                               layout.scroll.is_some()
                                               || layout.outlines
                                                        .as_ref()
                                                        .is_some_and(|grid| grid.dividers.is_some())
                                           });
        if !compound
        {
            return Ok(false);
        }
        let mut id = call.id;
        loop
        {
            let object = self.snapshot(id, owner)?;
            if object.definition.is_process_source() || object.state.process_dependencies()
            {
                return Ok(true);
            }
            id = self.next_in(call.id, id);
            if id == 0
            {
                return Ok(false);
            }
        }
    }

    fn snapshot_tracking(&self, call: &ControlCall, owner: u64, paint_pending: bool)
                         -> Result<TrackingSnapshot, Status>
    {
        let object = self.get(call.id, owner)?;
        let resolve = |id| crate::toolbox::runtime::Toolbox::control_definition(id);
        if object.state.splitter.is_some()
           || matches!(object.state.spec,
                       Spec::Standard { procedure: SPLITTER_PROCEDURE,
                                        .. })
        {
            return Ok(TrackingSnapshot::Split(self.snapshot_splitter(call, owner,
                                                                     paint_pending)?));
        }
        let splitter = if object.state
                                .layout
                                .as_ref()
                                .and_then(|layout| layout.outlines.as_ref())
                                .is_some_and(|grid| grid.dividers.is_some())
                          && call.operation == BEGIN_TRACKING
        {
            Some(self.snapshot_splitter(call, owner, paint_pending))
        }
        else
        {
            None
        };
        let scrolling = object.state.layout.as_ref().is_some_and(|layout| layout.scroll.is_some());
        let scroll = if scrolling
        {
            let mut wire = *call;
            Some(self.snapshot_scroll(&mut wire, owner, paint_pending, resolve))
        }
        else
        {
            None
        };
        let fallback = if scrolling
        {
            None
        }
        else
        {
            Some(RequestSnapshot { call:
                                       super::super::effects::DetachedCall::capture(call)?,
                                   next:           object.state.clone(),
                                   window:         object.window,
                                   tree:           object.tree,
                                   entry:          object.definition.reference(),
                                   inherited_clip: self.inherited_clip(call.id),
                                   layout_status:  self.layout_status(call.id),
                                   admitted_title: None })
        };
        Ok(TrackingSnapshot::Probe { initial: object.state.clone(),
                                     point: call.point,
                                     visible: self.visible_clip(call.id),
                                     splitter,
                                     scroll,
                                     fallback })
    }
}
