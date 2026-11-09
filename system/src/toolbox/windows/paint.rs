//! One canonical, pre-enqueue paint claim per actual User WIND. The compositor
//! owns its phase/clip cursor; this owner retains the current affine callback
//! even when a render transaction is invalidated or its temporary loan drops.
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::{ACCESS_DENIED, BAD_ABI, BAD_ARGUMENT, NO_MEMORY, NOT_FOUND, SERVICE_BUSY, Status};
use mac_object_table::domain::DomainId;
use spinning_top::Spinlock;

use super::*;
use crate::{machine::scheduler::accounting::AllocationAuthority,
            toolbox::resources::definitions::callbacks::{self, CompletionTarget, Notice, Pending,
                                                         packet::{Arguments, Object,
                                                                  PreparedSurface, Reply,
                                                                  Request, SurfacePlan,
                                                                  SurfaceSnapshot}}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaintDomain
{
    Frame,
    GrowIcon,
    TrackingGrow
}
/// Opaque geometry/source receipt for the real canonical Window. Content
/// revisions stay separate so an unrelated content-only update cannot stale
/// a partial transaction's structural readset. No pointer/ABI padding equality.
#[derive(Clone, Copy)]
pub(crate) struct WindowRenderStamp
{
    owner:  DomainId,
    window: Window
}
impl PartialEq for WindowRenderStamp
{
    fn eq(&self, other: &Self) -> bool
    {
        let a = &self.window;
        let b = &other.window;
        let source = match (a.source, b.source)
        {
            (WindowSource::Application, WindowSource::Application) => true,
            (WindowSource::System { resources: a,
                                    recovery: ar },
             WindowSource::System { resources: b,
                                    recovery: br }) => ar == br && a.same_source(b),
            _ => false
        };
        let x = &a.state;
        let y = &b.state;
        self.owner == other.owner
        && a.id == b.id
        && a.owner == b.owner
        && a.entry.same(b.entry)
        && source
        && a.bounds == b.bounds
        && a.procedure == b.procedure
        && a.min_width == b.min_width
        && a.min_height == b.min_height
        && a.user_bounds == b.user_bounds
        && a.standard_bounds == b.standard_bounds
        && a.grow_icon_clip == b.grow_icon_clip
        && a.content_outline == b.content_outline
        && a.control_outline == b.control_outline
        && a.hidden == b.hidden
        && a.stay_in_back == b.stay_in_back
        && x.content == y.content
        && x.structure == y.structure
        && x.close_box == y.close_box
        && x.grow_box == y.grow_box
        && x.active == y.active
        && x.close_pressed == y.close_pressed
        && x.title == y.title
        && x.title_length == y.title_length
        && x.zoomed == y.zoomed
        && x.zoom_box == y.zoom_box
        && x.zoom_pressed == y.zoom_pressed
        && x.structure_piece_count == y.structure_piece_count
        && x.structure_pieces == y.structure_pieces
    }
}
impl Eq for WindowRenderStamp
{
}

#[derive(Clone, Copy)]
pub(crate) struct PaintPlan
{
    id:                WindowId,
    owner:             DomainId,
    revision:          u64,
    /// Committed-content stamp. The manager's `revision` also advances for
    /// bookkeeping and for job admission, neither of which changes a frame.
    content_revision:  u64,
    window:            Window,
    domain:            PaintDomain,
    tracking:          Option<Tracking>,
    tracking_revision: Option<u64>,
    surface:           Option<SurfacePlan>,
    screen:            Rect
}
impl PaintPlan
{
    pub(crate) fn window_id(&self) -> WindowId
    {
        self.id
    }

    pub(crate) fn window(&self) -> Window
    {
        self.window
    }

    pub(crate) fn screen(&self) -> Rect
    {
        self.screen
    }

    pub(crate) fn storage_owner(&self) -> DomainId
    {
        self.owner
    }

    pub(crate) fn is_user(&self) -> Result<bool, Status>
    {
        crate::toolbox::resources::definitions::is_user(self.window.entry)
    }

    pub(crate) fn same_tracking(&self, other: &Self) -> bool
    {
        self.same_gesture(other) && self.tracking == other.tracking
    }

    pub(crate) fn same_gesture(&self, other: &Self) -> bool
    {
        self.domain == PaintDomain::TrackingGrow
        && other.domain == self.domain
        && self.id == other.id
        && self.owner == other.owner
        && self.revision == other.revision
        && self.screen == other.screen
        && self.tracking_revision == other.tracking_revision
        && WindowRenderStamp { owner:  self.owner,
                               window: self.window }
           == WindowRenderStamp { owner:  other.owner,
                                  window: other.window }
    }

    fn request(&self, surface: Option<SurfaceSnapshot>, calculated: Option<Window>)
               -> Result<Request, Status>
    {
        let mut window = calculated.unwrap_or(self.window);
        let (message, arguments) = match self.domain
        {
            PaintDomain::Frame => (definition::DRAW, Arguments::Scalar(0)),
            PaintDomain::GrowIcon => (definition::DRAW_GROW_ICON, Arguments::Scalar(0)),
            PaintDomain::TrackingGrow =>
            {
                let Some(Tracking::Grow { window: id,
                                          preview,
                                          .. }) = self.tracking
                else
                {
                    return Err(BAD_ABI);
                };
                if id != self.id
                {
                    return Err(BAD_ABI);
                }
                if calculated.is_none()
                {
                    window.bounds = preview;
                    window.state.content = preview;
                    (definition::CALCULATE, Arguments::Scalar(0))
                }
                else
                {
                    (definition::GROW, Arguments::Rect(preview))
                }
            }
        };
        Ok(Request { properties: None,
                     kind: definition::WDEF,
                     id: self.window.procedure >> 4,
                     variation: self.window.procedure & 15,
                     message,
                     reflow: 0,
                     object: Object::Window(window.state),
                     arguments,
                     surface })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaintFailure
{
    Service(Status),
    Enqueue(callbacks::EnqueueError)
}
impl From<Status> for PaintFailure
{
    fn from(status: Status) -> Self
    {
        Self::Service(status)
    }
}
impl PaintFailure
{
    pub(crate) fn status(self) -> Status
    {
        match self
        {
            Self::Service(s) => s,
            Self::Enqueue(c) => c.status()
        }
    }
}

struct Data
{
    plan:        PaintPlan,
    scratch:     Option<PreparedSurface>,
    snapshot:    Option<SurfaceSnapshot>,
    pending:     Option<Pending>,
    reply:       Option<Reply>,
    failure:     Option<PaintFailure>,
    attempted:   bool,
    replayed:    bool,
    // The actual CALCULATE reply stays owned until GROW has really drained.
    calculation: Option<Reply>,
    calculated:  Option<Window>,
    canvas:      super::canvas::CanvasClaim
}
struct State
{
    data:      Option<Data>,
    contender: bool
}
/// Allocated against the original WIND account before author NEW. No execution
/// pin is parked here. The original payload and the real render transaction
/// retain this same owner until all admitted author work has actually drained.
pub(crate) struct PaintOwner
{
    id:         WindowId,
    owner:      DomainId,
    allocation: AllocationAuthority,
    notice:     Arc<Notice>,
    available:  Arc<Notice>,
    active:     AtomicBool,
    guardian:   super::canvas::WindowCanvasGuardian,
    state:      Spinlock<State>
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaintProgress
{
    Pending,
    Ready,
    Cancelled
}
#[must_use = "restore the same Window paint stage or finish it after actual author drain"]
pub(crate) struct PaintLoan
{
    owner:    Arc<PaintOwner>,
    data:     Option<Data>,
    finished: bool
}
impl PaintOwner
{
    pub(super) fn prepare(id: WindowId, owner: DomainId, allocation: AllocationAuthority,
                          notice: Arc<Notice>, guardian: super::canvas::WindowCanvasGuardian)
                          -> Result<Arc<Self>, Status>
    {
        if allocation.owner() != owner
           || guardian.window_id() != id
           || guardian.storage_owner() != owner
        {
            return Err(ACCESS_DENIED);
        }
        let available = Notice::linked(notice.clone())?;
        Arc::try_new(Self { id,
                            owner,
                            allocation,
                            notice,
                            available,
                            active: AtomicBool::new(false),
                            guardian,
                            state: Spinlock::new(State { data:      None,
                                                         contender: false }) }).map_err(|_| {
                                                                                   NO_MEMORY
                                                                               })
    }

    pub(crate) fn active(&self) -> bool
    {
        self.active.load(Ordering::Acquire)
    }

    pub(crate) fn cancelled(&self) -> bool
    {
        self.guardian.cancelled()
    }

    /// Pure current-owner work hint, never an author ACK. Consume BEFORE
    /// servicing a turn so an actual concurrent completion remains visible.
    pub(crate) fn ready(&self) -> bool
    {
        self.available.ready()
    }

    pub(crate) fn take_ready(&self) -> bool
    {
        self.available.take()
    }

    pub(crate) fn cancel(&self)
    {
        if self.active()
        {
            self.guardian.cancel();
        }
    }

    pub(crate) fn checkout(self: &Arc<Self>) -> Result<PaintLoan, Status>
    {
        InvocationGate::run(|| {
            let mut state = self.state.lock();
            if !self.active()
            {
                return Err(NOT_FOUND);
            }
            let Some(data) = state.data.take()
            else
            {
                state.contender = true;
                return Err(SERVICE_BUSY);
            };
            Ok(PaintLoan { owner:    self.clone(),
                           data:     Some(data),
                           finished: false })
        })
    }
}
impl PaintLoan
{
    pub(crate) fn plan(&self) -> Result<PaintPlan, Status>
    {
        self.data.as_ref().map(|data| data.plan).ok_or(BAD_ABI)
    }

    /// Guard-free. Capacity refusal leaves the same original plan/claim; no
    /// allocation fallback or ambient caller allocation authority is used.
    pub(crate) fn prepare(&mut self) -> Result<(), Status>
    {
        let data = self.data.as_mut().ok_or(BAD_ABI)?;
        if self.owner.cancelled()
        {
            return Ok(());
        }
        if data.plan.domain == PaintDomain::TrackingGrow && data.calculated.is_none()
        {
            return Ok(());
        }
        if data.scratch.is_some() || data.snapshot.is_some() || data.attempted
        {
            return Ok(());
        }
        let result = crate::allocator::with_target(crate::allocator::Target::Owner {
            account: self.owner.allocation.clone(),
            class: mac_kernel_model::policy::MemoryClass::Kernel
        }, || data.plan.surface.ok_or(BAD_ARGUMENT)?.prepare().map_err(|(status, _)| status));
        match result
        {
            Ok(scratch) =>
            {
                data.scratch = Some(scratch);
                Ok(())
            }
            Err(status) => Err(status)
        }
    }

    /// Guard-free. A cancelled owner still polls its SAME admitted Pending to
    /// actual RETURN/stopped before releasing the target claim. Pending::Drop
    /// alone is deliberately not treated as author completion.
    pub(crate) fn advance(&mut self) -> Result<PaintProgress, PaintFailure>
    {
        let data = self.data.as_mut().ok_or(BAD_ABI)?;
        if let Some(pending) = data.pending.as_ref()
        {
            match pending.poll()
            {
                Ok(Some(reply)) =>
                {
                    data.reply = Some(reply);
                    drop(data.pending.take());
                }
                Ok(None) => return Ok(PaintProgress::Pending),
                Err(status) =>
                {
                    if pending.ready()
                    {
                        drop(data.pending.take());
                    }
                    data.failure = Some(status.into());
                }
            }
        }
        if self.owner.cancelled() && data.pending.is_none()
        {
            return Ok(PaintProgress::Cancelled);
        }
        if let Some(status) = data.failure
        {
            return Err(status);
        }
        if data.reply.is_some()
        {
            return Ok(PaintProgress::Ready);
        }
        if data.attempted
        {
            return Err(BAD_ABI.into());
        }
        let calculating =
            data.plan.domain == PaintDomain::TrackingGrow && data.calculated.is_none();
        let snapshot =
            if calculating { None } else { Some(data.snapshot.take().ok_or(BAD_ARGUMENT)?) };
        let request = data.plan.request(snapshot, data.calculated)?;
        // All state is owned by this loan, while the canonical active claim
        // remains visible. A simultaneous cancel cannot admit DISPOSE yet.
        data.attempted = true;
        let result =
            callbacks::enqueue_for_object(data.plan.window.entry,
                                          request,
                                          CompletionTarget::System(self.owner.available.clone()),
                                          &self.owner.allocation);
        match result
        {
            Ok(pending) =>
            {
                data.pending = Some(pending);
                Ok(PaintProgress::Pending)
            }
            Err(cause) =>
            {
                let failure = PaintFailure::Enqueue(cause);
                data.failure = Some(failure);
                Err(failure)
            }
        }
    }

    pub(crate) fn author_failure(&self) -> Option<Status>
    {
        self.data
            .as_ref()
            .and_then(|data| data.reply.as_ref())
            .filter(|reply| reply.result < 0)
            .map(|reply| i32::try_from(reply.result).unwrap_or(BAD_ABI))
    }

    pub(crate) fn tracking_window(&self) -> Result<Window, Status>
    {
        self.data.as_ref().and_then(|data| data.calculated).ok_or(BAD_ARGUMENT)
    }

    /// Guard-free final retirement. Only accepted replay, explicit cancellation
    /// with genuine broker drain, or a never-enqueued failure may release it.
    /// Source/packet readers drop BEFORE publishing real availability/wake.
    pub(crate) fn finish(&mut self) -> Result<(), Status>
    {
        let data = self.data.as_ref().ok_or(BAD_ABI)?;
        if data.pending.is_some() || (!data.replayed && !self.owner.cancelled())
        {
            return Err(SERVICE_BUSY);
        }
        drop(self.data.take());
        self.finished = true;
        Ok(())
    }
}
impl Drop for PaintLoan
{
    fn drop(&mut self)
    {
        let wake = InvocationGate::run(|| {
            let mut state = self.owner.state.lock();
            state.data = self.data.take();
            if self.finished
            {
                self.owner.active.store(false, Ordering::Release);
            }
            core::mem::take(&mut state.contender)
        });
        if self.finished
        {
            // Actual owned-reader/claim return, never an ACK or a poll retry.
            crate::kernel::window_sources_changed();
        }
        if self.finished || wake
        {
            self.owner.available.request();
        }
    }
}
impl WindowManager
{
    /// Original gesture and canonical Window/source receipt. No surface or
    /// guessed preview expansion is admitted before the real CALCULATE reply.
    pub(crate) fn plan_tracking_grow(&self) -> Result<Option<PaintPlan>, Status>
    {
        let Some(tracking @ Tracking::Grow { window: id,
                                  preview,
                                  .. }) = self.tracking
        else
        {
            return Ok(None);
        };
        let (owner, owned) = self.windows
                                 .get_with_owner(id.0)
                                 .filter(|(_, owned)| owned.initialized && !owned.hidden)
                                 .ok_or(NOT_FOUND)?;
        if !owned.binding.matches(owned.entry)
        {
            return Err(BAD_ABI);
        }
        let mut proposed = owned.window;
        proposed.bounds = preview;
        proposed.state.content = preview;
        super::operations::validate_bounds(&proposed)?;
        Ok(Some(PaintPlan { id,
                            owner,
                            revision: owned.revision.ok_or(BAD_ABI)?,
                            content_revision: owned.content_revision.ok_or(BAD_ABI)?,
                            window: self.paint_window(id)?,
                            domain: PaintDomain::TrackingGrow,
                            tracking: Some(tracking),
                            tracking_revision: Some(self.tracking_revision.ok_or(BAD_ABI)?),
                            surface: None,
                            screen: self.screen }))
    }

    pub(crate) fn cancel_tracking_grow(&mut self, plan: &PaintPlan) -> bool
    {
        if plan.domain == PaintDomain::TrackingGrow
           && self.tracking == plan.tracking
           && self.tracking_revision == plan.tracking_revision
        {
            self.tracking = None;
            true
        }
        else
        {
            false
        }
    }

    pub(crate) fn validate_tracking_grow(&self, plan: &PaintPlan) -> Result<(), Status>
    {
        if plan.domain != PaintDomain::TrackingGrow
        {
            return Err(BAD_ARGUMENT);
        }
        self.validate_paint_plan(plan)
    }

    pub(crate) fn accept_tracking_calculation(&self, loan: &mut PaintLoan) -> Result<Rect, Status>
    {
        self.validate_paint_loan(loan)?;
        let data = loan.data.as_mut().ok_or(BAD_ABI)?;
        if data.plan.domain != PaintDomain::TrackingGrow
        {
            return Err(BAD_ARGUMENT);
        }
        if let Some(window) = data.calculated
        {
            return Ok(window.structure());
        }
        let reply = data.reply.as_ref().ok_or(SERVICE_BUSY)?;
        if reply.result < 0
        {
            return Err(i32::try_from(reply.result).unwrap_or(BAD_ABI));
        }
        let mut window = data.plan.window;
        let Some(Tracking::Grow { preview,
                                  .. }) = data.plan.tracking
        else
        {
            return Err(BAD_ABI);
        };
        window.bounds = preview;
        window.state.content = preview;
        let state = reply.window()?;
        super::operations::validate_state(&state, &window)?;
        window.state = state;
        data.calculated = Some(window);
        data.calculation = data.reply.take();
        data.attempted = false;
        Ok(window.structure())
    }

    pub(crate) fn set_tracking_surface(&self, loan: &mut PaintLoan, canvas: &Canvas<'_>)
                                       -> Result<(), Status>
    {
        self.validate_paint_loan(loan)?;
        let data = loan.data.as_mut().ok_or(BAD_ABI)?;
        let target = data.calculated.ok_or(BAD_ARGUMENT)?.structure();
        if data.plan.surface.is_none()
        {
            data.plan.surface = Some(SurfacePlan::new(canvas, target)?);
        }
        Ok(())
    }

    /// The trusted source uses the identical geometry validator and crop
    /// contract, but needs no user broker packet or asynchronous return.
    pub(crate) fn calculate_trusted_tracking(&self, plan: &PaintPlan) -> Result<Window, Status>
    {
        self.validate_tracking_grow(plan)?;
        if crate::toolbox::resources::definitions::is_user(plan.window.entry)?
        {
            return Err(BAD_ARGUMENT);
        }
        let Some(Tracking::Grow { preview,
                                  .. }) = plan.tracking
        else
        {
            return Err(BAD_ABI);
        };
        let mut window = plan.window;
        window.bounds = preview;
        window.state.content = preview;
        let status =
            window.source.with(window.owner, || {
                              window.call(definition::CALCULATE, 0, core::ptr::null_mut(), None)
                          })?;
        if status < 0
        {
            return Err(i32::try_from(status).unwrap_or(BAD_ABI));
        }
        super::operations::validate_state(&window.state, &window)?;
        Ok(window)
    }

    pub(crate) fn draw_trusted_tracking(&self, plan: &PaintPlan, mut calculated: Window,
                                        canvas: &mut Canvas<'_>)
                                        -> Result<(), Status>
    {
        self.validate_tracking_grow(plan)?;
        let Some(Tracking::Grow { preview,
                                  .. }) = plan.tracking
        else
        {
            return Err(BAD_ABI);
        };
        let mut surface = Surface::from_canvas(canvas);
        let status = calculated.source.with(calculated.owner, || {
                                           calculated.call(definition::GROW,
                                                           (&preview as *const Rect) as i64,
                                                           &mut surface,
                                                           None)
                                       })?;
        if status < 0 { Err(i32::try_from(status).unwrap_or(BAD_ABI)) } else { Ok(()) }
    }

    /// Read with the actual ordered Window list under the same Desktop
    /// exclusion. Z-order is that real list, not a global HIT/input revision.
    /// The caller records content_revision(id) separately only where relevant.
    pub(crate) fn render_stamp(&self, id: WindowId) -> Result<WindowRenderStamp, Status>
    {
        let (owner, owned) = self.windows
                                 .get_with_owner(id.0)
                                 .filter(|(_, owned)| owned.initialized)
                                 .ok_or(NOT_FOUND)?;
        if !owned.binding.matches(owned.entry)
        {
            return Err(BAD_ABI);
        }
        owned.revision.filter(|revision| *revision != u64::MAX).ok_or(BAD_ABI)?;
        owned.content_revision.filter(|revision| *revision != u64::MAX).ok_or(BAD_ABI)?;
        Ok(WindowRenderStamp { owner,
                               window: owned.window })
    }

    fn paint_tracking(&self, id: WindowId, domain: PaintDomain) -> Option<Tracking>
    {
        match self.tracking
        {
            Some(value @ (Tracking::Close { window,
                                    .. }
                 | Tracking::Zoom { window,
                                   .. }))
                if domain != PaintDomain::TrackingGrow && window == id =>
            {
                Some(value)
            }
            Some(value @ Tracking::Grow { window,
                                  .. })
                if domain == PaintDomain::TrackingGrow && window == id =>
            {
                Some(value)
            }
            _ => None
        }
    }

    fn paint_window(&self, id: WindowId) -> Result<Window, Status>
    {
        let owned = self.windows
                        .get(id.0)
                        .filter(|owned| owned.initialized && !owned.hidden)
                        .ok_or(NOT_FOUND)?;
        if !owned.binding.matches(owned.entry)
        {
            return Err(BAD_ABI);
        }
        let mut window = owned.window;
        window.state.close_pressed = u32::from(matches!(self.tracking,
            Some(Tracking::Close { window, armed: true }) if window == id));
        window.state.zoom_pressed = u32::from(matches!(self.tracking,
            Some(Tracking::Zoom { window, armed: true }) if window == id));
        Ok(window)
    }

    /// Original trusted WDEF execution for one admitted compositor phase.
    /// The caller retains the normal resource scope and exact clipped Canvas.
    /// User authors are never called synchronously from this route.
    pub(crate) fn draw_trusted_paint(&self, id: WindowId, domain: PaintDomain,
                                     canvas: &mut Canvas<'_>)
                                     -> Result<bool, Status>
    {
        let mut window = self.paint_window(id)?;
        if crate::toolbox::resources::definitions::is_user(window.entry)?
        {
            return Ok(false);
        }
        let (message, parameter) = match domain
        {
            PaintDomain::Frame => (definition::DRAW, 0),
            PaintDomain::GrowIcon => (definition::DRAW_GROW_ICON, 0),
            PaintDomain::TrackingGrow => return Err(BAD_ARGUMENT)
        };
        let original_origin = canvas.origin();
        let original_clip = canvas.clip();
        if domain == PaintDomain::GrowIcon
           && let Some(mut grow) = window.grow_icon_clip
        {
            let offset = |a: i16, b: i16, c: i16| {
                a.checked_add(b).and_then(|a| a.checked_add(c)).ok_or(BAD_ARGUMENT)
            };
            grow = Rect::new(offset(grow.left, window.bounds.left, original_origin.h)?,
                             offset(grow.top, window.bounds.top, original_origin.v)?,
                             offset(grow.right, window.bounds.left, original_origin.h)?,
                             offset(grow.bottom, window.bounds.top, original_origin.v)?);
            let mut clipped = Rect::ZERO;
            if !sect_rect(grow, original_clip, &mut clipped)
            {
                return Ok(true);
            }
            // set_clip consumes local coordinates. Select/restore its physical
            // rectangle at zero origin, preserving the caller's actual port.
            canvas.set_origin(0, 0);
            canvas.set_clip(clipped);
            canvas.set_origin(original_origin.h, original_origin.v);
        }
        let mut surface = Surface::from_canvas(canvas);
        let result = window.call(message, parameter, &mut surface, None);
        canvas.set_origin(0, 0);
        canvas.set_clip(original_clip);
        canvas.set_origin(original_origin.h, original_origin.v);
        if result < 0
        {
            return Err(i32::try_from(result).unwrap_or(BAD_ABI));
        }
        Ok(true)
    }

    /// Geometry/close must wait for the current admitted paint stage. Geometry
    /// cancellation invalidates its pixels, but never simulates author drain.
    pub(super) fn exclude_paint(&self, id: WindowId) -> Result<(), Status>
    {
        self.windows.get(id.0).ok_or(NOT_FOUND)?.canvas_guardian()?.exclude_canvas()
    }

    pub(crate) fn plan_paint(&self, id: WindowId, domain: PaintDomain, canvas: &Canvas<'_>)
                             -> Result<Option<PaintPlan>, Status>
    {
        let (owner, owned) = self.windows
                                 .get_with_owner(id.0)
                                 .filter(|(_, owned)| owned.initialized && !owned.hidden)
                                 .ok_or(NOT_FOUND)?;
        if !owned.binding.matches(owned.entry)
        {
            return Err(BAD_ABI);
        }
        if !crate::toolbox::resources::definitions::is_user(owned.entry)?
        {
            return Ok(None);
        }
        if owned.paint.as_ref().is_some_and(|owner| owner.guardian.geometry_active())
        {
            return Err(SERVICE_BUSY);
        }
        let window = self.paint_window(id)?;
        let target = match domain
        {
            PaintDomain::Frame => window.structure(),
            PaintDomain::GrowIcon =>
            {
                let mut target = window.structure();
                if let Some(mut grow) = window.grow_icon_clip
                {
                    grow.offset(window.bounds.left, window.bounds.top);
                    if !sect_rect(target, grow, &mut target)
                    {
                        return Ok(None);
                    }
                }
                target
            }
            PaintDomain::TrackingGrow => return Err(BAD_ARGUMENT)
        };
        let origin = canvas.origin();
        let physical = Rect::new(target.left.checked_add(origin.h).ok_or(BAD_ARGUMENT)?,
                                 target.top.checked_add(origin.v).ok_or(BAD_ARGUMENT)?,
                                 target.right.checked_add(origin.h).ok_or(BAD_ARGUMENT)?,
                                 target.bottom.checked_add(origin.v).ok_or(BAD_ARGUMENT)?);
        let mut intersect = Rect::ZERO;
        if !sect_rect(physical, canvas.clip(), &mut intersect)
        {
            return Ok(None);
        }
        Ok(Some(PaintPlan { id,
                            owner,
                            revision: owned.revision.ok_or(BAD_ABI)?,
                            content_revision: owned.content_revision.ok_or(BAD_ABI)?,
                            window,
                            domain,
                            tracking: self.paint_tracking(id, domain),
                            tracking_revision: None,
                            surface: Some(SurfacePlan::new(canvas, target)?),
                            screen: self.screen }))
    }

    fn validate_paint_plan(&self, plan: &PaintPlan) -> Result<(), Status>
    {
        let (owner, owned) = self.windows
                                 .get_with_owner(plan.id.0)
                                 .filter(|(_, owned)| owned.initialized && !owned.hidden)
                                 .ok_or(NOT_FOUND)?;
        if self.screen != plan.screen
           || owner != plan.owner
           || owned.content_revision != Some(plan.content_revision)
           || !plan.window.draws_same_frame_as(&owned.window)
           || !owned.binding.matches(plan.window.entry)
        {
            return Err(SERVICE_BUSY);
        }
        if self.paint_tracking(plan.id, plan.domain) != plan.tracking
           || (plan.domain == PaintDomain::TrackingGrow
               && self.tracking_revision != plan.tracking_revision)
        {
            return Err(SERVICE_BUSY);
        }
        Ok(())
    }

    pub(crate) fn admit_paint(&self, plan: PaintPlan) -> Result<Arc<PaintOwner>, Status>
    {
        self.validate_paint_plan(&plan)?;
        let owner =
            self.windows.get(plan.id.0).and_then(|owned| owned.paint.as_ref()).ok_or(BAD_ABI)?;
        if owner.id != plan.id
           || owner.owner != plan.owner
           || !Arc::ptr_eq(&owner.notice, &self.work_notice)
        {
            return Err(ACCESS_DENIED);
        }
        InvocationGate::run(|| {
            let mut state = owner.state.lock();
            if owner.active() || owner.guardian.geometry_active() || state.data.is_some()
            {
                return Err(SERVICE_BUSY);
            }
            let canvas = self.claim_canvas(&owner.guardian, owner.available.clone())?;
            owner.available.take();
            state.data = Some(Data { plan,
                                     scratch: None,
                                     snapshot: None,
                                     pending: None,
                                     reply: None,
                                     failure: None,
                                     attempted: false,
                                     replayed: false,
                                     calculation: None,
                                     calculated: None,
                                     canvas });
            owner.active.store(true, Ordering::Release);
            // Actual admission creates the first real current-clip work edge.
            owner.available.request();
            Ok(owner.clone())
        })
    }

    fn validate_paint_loan(&self, loan: &PaintLoan) -> Result<(), Status>
    {
        let plan = loan.plan()?;
        self.validate_paint_plan(&plan)?;
        let owner =
            self.windows.get(plan.id.0).and_then(|owned| owned.paint.as_ref()).ok_or(NOT_FOUND)?;
        self.validate_canvas_claim(&loan.data.as_ref().ok_or(BAD_ABI)?.canvas)?;
        if !Arc::ptr_eq(owner, &loan.owner) || !owner.active() || owner.cancelled()
        {
            return Err(SERVICE_BUSY);
        }
        Ok(())
    }

    /// Root validates its retained semantic dependency readset in this same
    /// actual render domain BEFORE invoking capture or replay. No Canvas borrow
    /// or pointer is retained across guard-free preparation/author scheduling.
    pub(crate) fn capture_paint(&self, loan: &mut PaintLoan, canvas: &Canvas<'_>)
                                -> Result<(), Status>
    {
        self.validate_paint_loan(loan)?;
        let data = loan.data.as_mut().ok_or(BAD_ABI)?;
        if data.attempted || data.snapshot.is_some()
        {
            return Err(BAD_ARGUMENT);
        }
        let scratch = data.scratch.take().ok_or(BAD_ARGUMENT)?;
        match scratch.capture(canvas)
        {
            Ok(snapshot) =>
            {
                data.snapshot = Some(snapshot);
                Ok(())
            }
            Err((status, scratch)) =>
            {
                data.scratch = Some(scratch);
                Err(status)
            }
        }
    }

    pub(crate) fn replay_paint(&self, loan: &mut PaintLoan, canvas: &mut Canvas<'_>)
                               -> Result<Rect, Status>
    {
        self.validate_paint_loan(loan)?;
        let data = loan.data.as_mut().ok_or(BAD_ABI)?;
        if data.replayed
        {
            return Err(BAD_ARGUMENT);
        }
        let reply = data.reply.as_ref().ok_or(SERVICE_BUSY)?;
        if reply.result < 0
        {
            return Err(i32::try_from(reply.result).unwrap_or(BAD_ABI));
        }
        // DRAW results never replace canonical WDEF geometry/state.
        let changed = reply.paint(canvas)?;
        data.replayed = true;
        Ok(changed)
    }
}
