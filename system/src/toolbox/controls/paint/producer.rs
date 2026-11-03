//! Owned retained CDEF paint. Root keeps this canonical owner until the SAME
//! admitted callback drains; losing a temporary loan never means author ACK.
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::{ACCESS_DENIED, BAD_ABI, BAD_ARGUMENT, NO_MEMORY, NOT_FOUND, NOT_SUPPORTED,
              SERVICE_BUSY, Status,
              definitions::{CDEF, DRAW},
              geometry::Rect};
use mac_object_table::{domain::DomainKind, shared::InvocationGate as _};
use spinning_top::Spinlock;

use super::{PaintOrigin, PaintPlan, UnclaimedSubject};
use crate::{machine::scheduler::accounting::AllocationAuthority,
            toolbox::{controls::{objects::{ManagedPaintClaim, Manager},
                                 scrollbox::{Manager as ScrollBoxManager, ScrollBoxPaintClaim}},
                      handles::InvocationGate,
                      quickdraw::Canvas,
                      resources::definitions::callbacks::{self, CompletionTarget, Notice,
                                                          Pending,
                                                          packet::{Arguments, PreparedSurface,
                                                                   Reply, Request,
                                                                   SurfaceSnapshot}},
                      windows::manager::{WindowManager, canvas::CanvasClaim}}};

/// Original failure identity, independent of the eventual wire status projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaintFailure
{
    Admission(Status),
    Prepare(Status),
    Capture(Status),
    Validation(Status),
    Enqueue(Status),
    Poll(Status),
    Author(i64),
    Replay(Status),
    State(Status)
}
impl PaintFailure
{
    pub(crate) fn status(self) -> Status
    {
        match self
        {
            Self::Admission(s)
            | Self::Prepare(s)
            | Self::Capture(s)
            | Self::Validation(s)
            | Self::Enqueue(s)
            | Self::Poll(s)
            | Self::Replay(s)
            | Self::State(s) => s,
            Self::Author(result) => Status::try_from(result).unwrap_or(BAD_ABI)
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaintProgress
{
    Pending,
    Ready,
    Cancelled,
    Failed(PaintFailure)
}

/// Genuine object lifetime/readset, never a raw correlation id. SBOX is an
/// optimistic readset: holding it does not block ordinary mutation or ACK.
pub(crate) enum PaintClaim
{
    Managed(ManagedPaintClaim),
    ScrollBox(ScrollBoxPaintClaim)
}

/// SAME elected inputs returned on factory refusal, before any enqueue. Drop
/// outside Desktop or retry under the original real allocation/recovery scope.
pub(crate) struct PaintInputs
{
    plan:       PaintPlan,
    allocation: AllocationAuthority,
    notice:     Arc<Notice>,
    claim:      Option<PaintClaim>,
    canvas:     PaintTarget
}
impl PaintInputs
{
    fn validate(&self) -> Result<(), Status>
    {
        if !self.canvas.observes(&self.notice)
           || self.canvas.storage_owner() != self.allocation.owner()
           || !self.plan.source().is_process_source()
        {
            return Err(ACCESS_DENIED);
        }
        PaintOwner::check_policy(&self.plan)?;
        match (self.plan.subject(), self.plan.origin(), &self.claim)
        {
            (UnclaimedSubject::Stateless, PaintOrigin::Stateless, None) => Ok(()),
            (UnclaimedSubject::Control(id),
             PaintOrigin::Managed(origin),
             Some(PaintClaim::Managed(claim)))
                if matches!(self.canvas, PaintTarget::Window(_))
                   && id == origin.control_id()
                   && *claim.origin() == origin
                   && self.canvas.matches_window(origin.window())
                   && origin.storage_owner() == self.canvas.storage_owner() =>
            {
                Ok(())
            }
            (UnclaimedSubject::ScrollBox(id),
             PaintOrigin::ScrollBox(origin),
             Some(PaintClaim::ScrollBox(claim)))
                if id == origin.scroll_box_id()
                   && claim.origin() == origin
                   && origin.source_matches(self.plan.source())
                   && self.canvas.matches_window(origin.window())
                   && origin.storage_owner() == self.canvas.storage_owner() =>
            {
                Ok(())
            }
            _ => Err(ACCESS_DENIED)
        }
    }
}

/// Target lifetime is independent of CDEF source/object identity. A checked
/// caller surface is never represented by an invented Window/CTRL key.
enum PaintTarget
{
    Window(CanvasClaim),
    Surface(crate::machine::traps::user::surface::Origin)
}
impl PaintTarget
{
    fn observes(&self, notice: &Arc<Notice>) -> bool
    {
        match self
        {
            Self::Window(c) => c.observes(notice),
            Self::Surface(s) => s.observes(notice)
        }
    }

    fn storage_owner(&self) -> mac_object_table::domain::DomainId
    {
        match self
        {
            Self::Window(c) => c.storage_owner(),
            Self::Surface(s) => s.owner()
        }
    }

    fn matches_window(&self, window: u64) -> bool
    {
        match self
        {
            Self::Window(c) => c.window_id().0 == window,
            // SBOX's independent full original key/readset authenticates its
            // actual window. The caller surface supplies no Window authority.
            Self::Surface(_) => true
        }
    }

    fn cancelled(&self) -> bool
    {
        match self
        {
            Self::Window(c) => c.cancelled(),
            Self::Surface(s) => s.cancelled()
        }
    }

    fn completion(&self, notice: Arc<Notice>) -> CompletionTarget
    {
        match self
        {
            Self::Window(_) => CompletionTarget::System(notice),
            Self::Surface(origin) =>
            {
                CompletionTarget::ThreadAndSystem { thread: origin.thread(),
                                                    notice }
            }
        }
    }

    fn validate(&self, windows: &WindowManager) -> Result<(), Status>
    {
        match self
        {
            Self::Window(c) => windows.validate_canvas_claim(c),
            Self::Surface(s) if !s.cancelled() => Ok(()),
            Self::Surface(_) => Err(mac_abi::OPERATION_CANCELLED)
        }
    }
}

struct Data
{
    // Actual packet/scratch/readers precede retained source and object claims.
    pending:   Option<Pending>,
    reply:     Option<Reply>,
    snapshot:  Option<SurfaceSnapshot>,
    scratch:   Option<PreparedSurface>,
    arguments: Option<Arguments>,
    failure:   Option<PaintFailure>,
    attempted: bool,
    replayed:  bool,
    plan:      PaintPlan,
    claim:     Option<PaintClaim>,
    // Last, including cancellation: no Window reuse before real author drain.
    canvas:    PaintTarget
}
struct State
{
    data:      Option<Data>,
    contender: bool
}
/// Root installs this Arc in its canonical render/debt holder BEFORE advance
/// may enqueue. It is not a temporary Context owner or an autonomous registry.
/// The supplied Notice is owner-specific, already admitted, and must be the
/// SAME observer used for this CanvasClaim (or Root's single shared consumer).
#[must_use = "retain the canonical owner until accepted replay or actual drain"]
pub(crate) struct PaintOwner
{
    state:      Spinlock<State>,
    allocation: AllocationAuthority,
    notice:     Arc<Notice>,
    cancelled:  AtomicBool,
    finished:   AtomicBool
}
#[must_use = "restore this same stage or finish only after actual author drain"]
pub(crate) struct PaintLoan
{
    owner: Arc<PaintOwner>,
    data:  Option<Data>
}
impl PaintOwner
{
    /// This slice admits ordinary Process-storage SBOX User drawing only.
    /// Presence of genuine original recovery scope, never owner inequality,
    /// determines refusal; no protected operation is retried as ordinary.
    pub(crate) fn check_policy(plan: &PaintPlan) -> Result<(), Status>
    {
        if let PaintOrigin::ScrollBox(origin) = plan.origin()
        {
            if origin.storage_owner().kind() != DomainKind::Process
            {
                return Err(NOT_SUPPORTED);
            }
            let protected =
                plan.with_scroll_box_scope(|| crate::context::recovery_activity().is_some())?;
            if protected
            {
                return Err(NOT_SUPPORTED);
            }
        }
        Ok(())
    }

    pub(crate) fn prepare(plan: PaintPlan, canvas: CanvasClaim, claim: Option<PaintClaim>,
                          allocation: AllocationAuthority, notice: Arc<Notice>)
                          -> Result<Arc<Self>, (PaintFailure, PaintInputs)>
    {
        Self::prepare_inputs(PaintInputs { plan,
                                           allocation,
                                           notice,
                                           claim,
                                           canvas: PaintTarget::Window(canvas) })
    }

    /// Only the elected checked Call can mint this target. Caller surface
    /// carries original buffer custody; managed CTRL targets remain Window-only.
    pub(crate) fn prepare_surface(plan: PaintPlan,
                                  surface: crate::machine::traps::user::surface::Origin,
                                  claim: Option<PaintClaim>, allocation: AllocationAuthority,
                                  notice: Arc<Notice>)
                                  -> Result<Arc<Self>, (PaintFailure, PaintInputs)>
    {
        Self::prepare_inputs(PaintInputs { plan,
                                           allocation,
                                           notice,
                                           claim,
                                           canvas: PaintTarget::Surface(surface) })
    }

    fn prepare_inputs(inputs: PaintInputs) -> Result<Arc<Self>, (PaintFailure, PaintInputs)>
    {
        if let Err(status) = inputs.validate()
        {
            return Err((PaintFailure::Admission(status), inputs));
        }
        let target =
            crate::allocator::Target::Owner { account: inputs.allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        // Only clones occupy the empty cell before its fallible allocation.
        // Refusal therefore returns the ORIGINAL affine plan/claims intact.
        let owner = match crate::allocator::with_target(target, || {
                  Arc::try_new(Self { state:      Spinlock::new(State { data:      None,
                                                                        contender: false }),
                                      allocation: inputs.allocation.clone(),
                                      notice:     inputs.notice.clone(),
                                      cancelled:  AtomicBool::new(false),
                                      finished:   AtomicBool::new(false) })
              })
        {
            Ok(owner) => owner,
            Err(_) => return Err((PaintFailure::Admission(NO_MEMORY), inputs))
        };
        let PaintInputs { plan,
                          claim,
                          canvas,
                          .. } = inputs;
        // Unpublished unique owner, no fallible work or semantic Drop in gate.
        owner.state.lock().data = Some(Data { pending: None,
                                              reply: None,
                                              snapshot: None,
                                              scratch: None,
                                              arguments: None,
                                              failure: None,
                                              attempted: false,
                                              replayed: false,
                                              plan,
                                              claim,
                                              canvas });
        Ok(owner)
    }

    pub(crate) fn active(&self) -> bool
    {
        !self.finished.load(Ordering::Acquire)
    }

    /// Only hints from this already-admitted observer; never an author ACK.
    pub(crate) fn ready(&self) -> bool
    {
        self.notice.ready()
    }

    pub(crate) fn take_ready(&self) -> bool
    {
        self.notice.take()
    }

    pub(crate) fn cancel(&self)
    {
        if self.active() && !self.cancelled.swap(true, Ordering::AcqRel)
        {
            self.notice.request();
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
            Ok(PaintLoan { owner: self.clone(),
                           data:  Some(data) })
        })
    }
}
impl PaintLoan
{
    fn data(&self) -> Result<&Data, PaintFailure>
    {
        self.data.as_ref().ok_or(PaintFailure::State(BAD_ABI))
    }

    pub(crate) fn cancelled(&self) -> Result<bool, PaintFailure>
    {
        Ok(self.owner.cancelled.load(Ordering::Acquire) || self.data()?.canvas.cancelled())
    }

    fn validate(&self, windows: &WindowManager, controls: &Manager,
                scroll_boxes: &ScrollBoxManager)
                -> Result<(), PaintFailure>
    {
        let data = self.data()?;
        if self.cancelled()?
        {
            return Err(PaintFailure::Validation(SERVICE_BUSY));
        }
        data.canvas.validate(windows).map_err(PaintFailure::Validation)?;
        match &data.claim
        {
            Some(PaintClaim::Managed(claim)) =>
            {
                controls.validate_managed_paint(claim).map_err(PaintFailure::Validation)?
            }
            Some(PaintClaim::ScrollBox(claim)) =>
            {
                data.plan
                    .with_scroll_box_scope(|| {
                        claim.with_scope(|| scroll_boxes.validate_scroll_box_paint(claim))
                    })
                    .and_then(core::convert::identity)
                    .and_then(core::convert::identity)
                    .map_err(PaintFailure::Validation)?
            }
            None => ()
        }
        Ok(())
    }

    /// Guard-free. Preserve every already-admitted piece on later refusal.
    pub(crate) fn prepare(&mut self) -> Result<(), PaintFailure>
    {
        if self.cancelled()?
        {
            return Ok(());
        }
        let data = self.data.as_mut().ok_or(PaintFailure::State(BAD_ABI))?;
        if data.attempted || data.failure.is_some()
        {
            return Ok(());
        }
        let target =
            crate::allocator::Target::Owner { account: self.owner.allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || {
            if data.arguments.is_none()
            {
                data.arguments = Some(data.plan.arguments().map_err(PaintFailure::Prepare)?);
            }
            if data.scratch.is_none() && data.snapshot.is_none()
            {
                match data.plan.surface().prepare()
                {
                    Ok(scratch) => data.scratch = Some(scratch),
                    Err((status, _original)) => return Err(PaintFailure::Prepare(status))
                }
            }
            Ok(())
        })
    }

    /// Under Desktop, AFTER Root validates the original committed-content
    /// readset in this SAME turn. This reconstructs only the original subclip;
    /// no Canvas/Drawing borrow is stored in the canonical owner.
    pub(crate) fn capture(&mut self, windows: &WindowManager, controls: &Manager,
                          scroll_boxes: &ScrollBoxManager, canvas: &mut Canvas<'_>)
                          -> Result<(), PaintFailure>
    {
        self.validate(windows, controls, scroll_boxes)?;
        let data = self.data.as_mut().ok_or(PaintFailure::State(BAD_ABI))?;
        if data.attempted || data.snapshot.is_some() || data.failure.is_some()
        {
            return Err(PaintFailure::State(BAD_ARGUMENT));
        }
        let Data { plan,
                   scratch,
                   snapshot,
                   .. } = data;
        plan.with_canvas(canvas, |canvas| {
                let prepared = scratch.take().ok_or(BAD_ARGUMENT)?;
                match prepared.capture(canvas)
                {
                    Ok(captured) =>
                    {
                        *snapshot = Some(captured);
                        Ok(())
                    }
                    Err((status, same)) =>
                    {
                        *scratch = Some(same);
                        Err(status)
                    }
                }
            })
            .map_err(PaintFailure::Capture)?
            .map_err(PaintFailure::Capture)
    }

    /// Guard-free. One actual callback attempt, even after failure/cancel.
    /// A cold Pending does not acquire the broker lock or replay any prefix.
    pub(crate) fn advance(&mut self) -> PaintProgress
    {
        let cancelled = match self.cancelled()
        {
            Ok(cancelled) => cancelled,
            Err(failure) => return PaintProgress::Failed(failure)
        };
        let Some(data) = self.data.as_mut()
        else
        {
            return PaintProgress::Failed(PaintFailure::State(BAD_ABI));
        };
        if let Some(pending) = &data.pending
        {
            if !pending.ready()
            {
                return PaintProgress::Pending;
            }
            match pending.poll()
            {
                Ok(Some(reply)) =>
                {
                    data.reply = Some(reply);
                    drop(data.pending.take());
                }
                Ok(None) => return PaintProgress::Pending,
                Err(status) =>
                {
                    // ready is the broker's real RETURN/stopped terminal bit.
                    // Preserve original status; never infer success from Busy.
                    data.failure.get_or_insert(PaintFailure::Poll(status));
                    drop(data.pending.take());
                }
            }
        }
        if let Some(failure) = data.failure
        {
            return PaintProgress::Failed(failure);
        }
        if let Some(reply) = &data.reply
           && reply.result < 0
        {
            let failure = PaintFailure::Author(reply.result);
            data.failure = Some(failure);
            return PaintProgress::Failed(failure);
        }
        if cancelled
        {
            return PaintProgress::Cancelled;
        }
        if data.reply.is_some()
        {
            return PaintProgress::Ready;
        }
        if data.attempted
        {
            let failure = PaintFailure::State(BAD_ABI);
            data.failure = Some(failure);
            return PaintProgress::Failed(failure);
        }
        let Some(snapshot) = data.snapshot.take()
        else
        {
            let failure = PaintFailure::State(BAD_ARGUMENT);
            data.failure = Some(failure);
            return PaintProgress::Failed(failure);
        };
        let Some(arguments) = data.arguments.take()
        else
        {
            data.snapshot = Some(snapshot);
            let failure = PaintFailure::State(BAD_ARGUMENT);
            data.failure = Some(failure);
            return PaintProgress::Failed(failure);
        };
        let reference = data.plan.source().reference();
        let request = Request { kind: CDEF,
                                id: data.plan.id(),
                                variation: data.plan.variation(),
                                message: DRAW,
                                reflow: data.plan.reflow(),
                                object: data.plan.object(),
                                arguments,
                                surface: Some(snapshot) };
        data.attempted = true;
        let result = callbacks::enqueue_for_object(reference,
                                                   request,
                                                   data.canvas
                                                       .completion(self.owner.notice.clone()),
                                                   &self.owner.allocation);
        match result
        {
            Ok(pending) =>
            {
                data.pending = Some(pending);
                PaintProgress::Pending
            }
            Err(status) =>
            {
                let failure = PaintFailure::Enqueue(status);
                data.failure = Some(failure);
                PaintProgress::Failed(failure)
            }
        }
    }

    /// Original authentic callback wait, borrowed only after checkout has
    /// released its metadata gate. No Pending clone or polling loop is needed.
    pub(crate) fn pending(&self) -> Result<Option<&Pending>, Status>
    {
        Ok(self.data.as_ref().ok_or(BAD_ABI)?.pending.as_ref())
    }

    /// Under Desktop after Root's SAME-turn semantic readset validation. The
    /// original source/state is replayed; current Object appearance is not used.
    pub(crate) fn replay(&mut self, windows: &WindowManager, controls: &Manager,
                         scroll_boxes: &ScrollBoxManager, canvas: &mut Canvas<'_>)
                         -> Result<Rect, PaintFailure>
    {
        self.validate(windows, controls, scroll_boxes)?;
        let data = self.data.as_mut().ok_or(PaintFailure::State(BAD_ABI))?;
        if data.replayed
        {
            return Err(PaintFailure::State(BAD_ARGUMENT));
        }
        if let Some(failure) = data.failure
        {
            return Err(failure);
        }
        let reply = data.reply.as_ref().ok_or(PaintFailure::State(SERVICE_BUSY))?;
        if reply.result < 0
        {
            let failure = PaintFailure::Author(reply.result);
            data.failure = Some(failure);
            return Err(failure);
        }
        match data.plan
                  .with_canvas(canvas, |canvas| reply.paint(canvas))
                  .map_err(PaintFailure::Replay)
                  .and_then(|result| result.map_err(PaintFailure::Replay))
        {
            Ok(changed) =>
            {
                data.replayed = true;
                Ok(changed)
            }
            Err(failure) =>
            {
                data.failure = Some(failure);
                Err(failure)
            }
        }
    }

    /// Guard-free. No Pending can be abandoned here as if it completed.
    /// Preparation refusal alone requires explicit cancellation to abort.
    pub(crate) fn finish(&mut self) -> Result<(), Status>
    {
        let cancelled = self.cancelled().map_err(PaintFailure::status)?;
        let data = self.data.as_ref().ok_or(BAD_ABI)?;
        if data.pending.is_some() || (!data.replayed && !cancelled && data.failure.is_none())
        {
            return Err(SERVICE_BUSY);
        }
        // Real reader/claim returns and their notifications happen outside gate.
        drop(self.data.take());
        self.owner.finished.store(true, Ordering::Release);
        Ok(())
    }
}
impl Drop for PaintLoan
{
    fn drop(&mut self)
    {
        // Unique checkout and private constructors make the canonical cell
        // empty for this loan's entire lifetime. Swap performs no destruction
        // under the metadata gate; cancellation is not an author ACK.
        let wake = InvocationGate::run(|| {
            let mut state = self.owner.state.lock();
            core::mem::swap(&mut state.data, &mut self.data);
            core::mem::take(&mut state.contender)
        });
        if wake
        {
            self.owner.notice.request();
        }
    }
}
