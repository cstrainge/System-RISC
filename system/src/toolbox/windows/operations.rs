//! Owned existing-WIND operations. Plans are minted only by the actual manager;
//! author work and destruction are guard-free. No caller-supplied Window copy
//! or definition reference can construct an admission claim.
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use mac_abi::{ACCESS_DENIED, BAD_ABI, BAD_ARGUMENT, NO_MEMORY, NOT_FOUND, SERVICE_BUSY, Status};
use mac_object_table::{domain::ReleasedResource, registry::CleanupFailure, store::Removed};
use spinning_top::Spinlock;

use super::*;
use crate::{context::ThreadId,
            toolbox::resources::definitions::{QuiescedDefinition, QuiescencePlan,
                                              callbacks::{self, CompletionTarget, Notice,
                                                          Pending, PreparedWindowCallback,
                                                          packet::{Arguments, Object, Reply,
                                                                   Request}}}};

#[derive(Clone, Copy)]
struct Seal
{
    id:       WindowId,
    owner:    mac_object_table::domain::DomainId,
    revision: u64,
    entry:    DefinitionRef
}
#[derive(Clone, Copy)]
enum Purpose
{
    Hit
    {
        point: Point,
        scene: u64,
        below: usize
    },
    Geometry
    {
        action:    Action,
        activate:  bool,
        standard:  Option<Option<Rect>>,
        execution: GeometryExecution
    },
    Dispose,
    Initialize(i16)
}
#[derive(Clone, Copy)]
enum GeometryExecution
{
    User,
    Trusted
}
/// Actual trusted CALCULATE state is not a user broker packet/RETURN.
/// Both results are retained by the same owned operation across layout refusal.
enum OperationReply
{
    User(Reply),
    Trusted
    {
        result: i64,
        state:  WindowDefinitionState
    }
}
impl OperationReply
{
    fn result(&self) -> i64
    {
        match self
        {
            Self::User(reply) => reply.result,
            Self::Trusted { result,
                            .. } => *result
        }
    }

    fn window(&self) -> Result<WindowDefinitionState, Status>
    {
        match self
        {
            Self::User(reply) => reply.window(),
            Self::Trusted { state,
                            .. } => Ok(*state)
        }
    }
}
/// Fixed-size snapshot only; allocating the callback job happens after Desktop
/// has been released. Revalidation is repeated when a close is admitted/at
/// final commit; a plan does not reserve a mutable Rust borrow.
#[derive(Clone, Copy)]
pub(crate) struct OperationPlan
{
    seal:              Seal,
    window:            Window,
    purpose:           Purpose,
    tracking:          Option<Tracking>,
    tracking_revision: Option<u64>
}
impl OperationPlan
{
    pub(crate) fn window_id(&self) -> WindowId
    {
        self.seal.id
    }

    pub(crate) fn storage_owner(&self) -> mac_object_table::domain::DomainId
    {
        self.seal.owner
    }

    pub(crate) fn geometry_needs_user(&self) -> bool
    {
        matches!(self.purpose,
                 Purpose::Geometry { execution: GeometryExecution::User,
                                     .. })
    }

    pub(crate) fn viewport(&self) -> Rect
    {
        self.window.local_bounds()
    }

    fn request(&self) -> Request
    {
        let (message, param) = match self.purpose
        {
            Purpose::Hit { point,
                           .. } =>
            {
                (definition::HIT,
                 i64::from(((point.v as u16 as u32) << 16) | point.h as u16 as u32))
            }
            Purpose::Geometry { .. } => (definition::CALCULATE, 0),
            Purpose::Dispose => (definition::DISPOSE, 0),
            Purpose::Initialize(message) => (message, 0)
        };
        Request { kind: definition::WDEF,
                  id: self.window.procedure >> 4,
                  variation: self.window.procedure & 15,
                  message,
                  reflow: 0,
                  object: Object::Window(self.window.state),
                  arguments: Arguments::Scalar(param),
                  surface: None }
    }
}

struct JobState
{
    pending:              Option<Pending>,
    prepared:             Option<PreparedWindowCallback>,
    reply:                Option<OperationReply>,
    failure:              Option<Status>,
    started:              bool,
    busy:                 bool,
    waiting:              bool,
    consumed:             bool,
    admitted:             bool,
    outcome:              Option<Outcome>,
    cleanup:              CleanupPhase,
    cleanup_reason:       Option<CleanupReason>,
    cleanup_waiting:      bool,
    cleanup_event:        bool,
    quiescence_plan:      Option<QuiescencePlan>,
    quiescence_proof:     Option<QuiescedDefinition>,
    quiescence_loaned:    bool,
    quiescence_contender: bool,
    commit_blocked:       bool,
    allocation_wait:      bool,
    activation_armed:     bool,
    activation_returned:  bool,
    paint_exclusion:      Option<super::canvas::GeometryClaim>,
    acknowledgement:      Option<super::ack::Receipt>
}
/// The manager retains this SAME job for a closing WIND. A syscall context
/// merely holds another handle; its Drop cannot cancel/replay DISPOSE or lose
/// the original ticket. All methods which poll/allocate run outside Desktop.
pub(crate) struct WindowJob
{
    pub(super) canvas:     super::canvas::CanvasState,
    plan:                  Spinlock<OperationPlan>,
    waiter:                Option<ThreadId>,
    notice:                Arc<Notice>,
    available:             Arc<Notice>,
    wake_pending:          AtomicBool,
    terminal_ready:        AtomicBool,
    needs_acknowledgement: AtomicBool,
    acknowledged:          AtomicBool,
    quiescence_waiting:    AtomicBool,
    ui_blocked:            AtomicBool,
    ui_claimed:            AtomicBool,
    state:                 Spinlock<JobState>
}
/// The original Context owns this handle, never a release ticket. Closing work
/// remains in WM after this handle is cancelled. Drop only posts a real hint.
#[must_use = "retain the original operation across callback suspension"]
pub(crate) struct WindowOperation
{
    job: Arc<WindowJob>
}
impl WindowOperation
{
    pub(crate) fn prepare(plan: OperationPlan, waiter: ThreadId, notice: Arc<Notice>)
                          -> Result<Self, Status>
    {
        WindowJob::prepare(plan, waiter, notice).map(|job| Self { job })
    }

    /// Real System input/maintenance customer; no synthetic ThreadId.
    pub(crate) fn prepare_system(plan: OperationPlan, notice: Arc<Notice>) -> Result<Self, Status>
    {
        WindowJob::prepare_inner(plan, None, notice).map(|job| Self { job })
    }

    pub(crate) fn job(&self) -> &Arc<WindowJob>
    {
        &self.job
    }

    pub(crate) fn advance(&self) -> Result<Progress, Status>
    {
        self.job.advance()
    }

    pub(crate) fn take_close_cleanup(&self) -> Option<CloseCleanup>
    {
        self.job.take_close_cleanup()
    }
}
impl Drop for WindowOperation
{
    fn drop(&mut self)
    {
        if matches!(self.job.plan().purpose, Purpose::Dispose)
           && self.job.with_state(|state| state.admitted && !state.consumed)
        {
            self.job.notice.request();
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome
{
    Closed(Result<WindowId, CloseError>),
    Quiesced(WindowId)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CleanupReason
{
    Author(i64),
    Quiesced
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum CleanupPhase
{
    BeforeReply,
    Pending,
    Claimed,
    Blocked,
    Done
}
/// One actual close UI tail. Dropping an unfinished receipt returns its SAME
/// debt to the canonical WM job; the Context never owns the WIND ticket.
#[must_use = "finish the original close UI tail or retain its debt"]
pub(crate) struct UserCloseCleanup
{
    job:      Arc<WindowJob>,
    returned: bool
}
impl UserCloseCleanup
{
    pub(crate) fn window_id(&self) -> WindowId
    {
        self.job.plan().seal.id
    }

    /// Original logical recipient captured from the canonical Window payload.
    pub(crate) fn owner(&self) -> u64
    {
        self.job.plan().window.owner
    }

    /// Full original storage domain, independently checked with slot/revision.
    pub(crate) fn storage_owner(&self) -> mac_object_table::domain::DomainId
    {
        self.job.plan().seal.owner
    }

    /// Arm before the real control/manager preflight, closing return-before-
    /// park. The receipt remains affine; this is not an ACK or caller authority.
    pub(crate) fn arm_wait(&self)
    {
        self.job.with_state(|state| state.cleanup_event = false);
        self.job.ui_blocked.store(true, Ordering::Release);
        self.job.available.take();
    }

    /// Actual control/manager admission refusal: retain the SAME canonical UI
    /// debt without self-notifying from this unchanged failed attempt.
    pub(crate) fn defer(mut self)
    {
        let changed =
            self.job.with_state(|state| {
                        let changed = core::mem::take(&mut state.cleanup_event);
                        state.cleanup =
                            if changed { CleanupPhase::Pending } else { CleanupPhase::Blocked };
                        state.cleanup_waiting = false;
                        changed
                    });
        self.job.ui_claimed.store(false, Ordering::Release);
        self.job.ui_blocked.store(!changed, Ordering::Release);
        if changed
        {
            self.job.wake_pending.store(true, Ordering::Release);
            self.job.available.request();
        }
        self.returned = true;
    }

    /// Call after the actual original child/scene/cursor cleanup tail succeeds.
    /// This records UI debt completion, never a Window or definition ACK.
    pub(crate) fn finish(mut self)
    {
        self.job.with_state(|state| state.cleanup = CleanupPhase::Done);
        self.job.ui_claimed.store(false, Ordering::Release);
        self.job.ui_blocked.store(false, Ordering::Release);
        self.job.wake_pending.store(true, Ordering::Release);
        self.returned = true;
        self.job.available.request();
    }
}
impl Drop for UserCloseCleanup
{
    fn drop(&mut self)
    {
        if !self.returned
        {
            let waiting = self.job.with_state(|state| {
                                      state.cleanup = CleanupPhase::Pending;
                                      core::mem::take(&mut state.cleanup_waiting)
                                  });
            self.job.ui_claimed.store(false, Ordering::Release);
            self.job.ui_blocked.store(false, Ordering::Release);
            // This is actual affine cancellation/availability. Ordinary
            // admission failures MUST use defer(), not this Drop path.
            self.job.notice.request();
            if waiting
            {
                self.job.wake_pending.store(true, Ordering::Release);
                self.job.available.request();
            }
        }
    }
}
/// One shared UI tail for actual User-author or trusted-author close work.
/// Each variant retains its original canonical owner; no caller owns WIND.
#[must_use = "finish the original close UI tail or retain its debt"]
pub(crate) enum CloseCleanup
{
    User(UserCloseCleanup),
    Trusted(super::trusted::TrustedCleanup)
}
impl CloseCleanup
{
    pub(crate) fn window_id(&self) -> WindowId
    {
        match self
        {
            Self::User(x) => x.window_id(),
            Self::Trusted(x) => x.window_id()
        }
    }

    pub(crate) fn owner(&self) -> u64
    {
        match self
        {
            Self::User(x) => x.owner(),
            Self::Trusted(x) => x.owner()
        }
    }

    pub(crate) fn storage_owner(&self) -> mac_object_table::domain::DomainId
    {
        match self
        {
            Self::User(x) => x.storage_owner(),
            Self::Trusted(x) => x.storage_owner()
        }
    }

    pub(crate) fn arm_wait(&self)
    {
        match self
        {
            Self::User(x) => x.arm_wait(),
            Self::Trusted(x) => x.arm_wait()
        }
    }

    pub(crate) fn defer(self)
    {
        match self
        {
            Self::User(x) => x.defer(),
            Self::Trusted(x) => x.defer()
        }
    }

    pub(crate) fn finish(self)
    {
        match self
        {
            Self::User(x) => x.finish(),
            Self::Trusted(x) => x.finish()
        }
    }
}

pub(crate) enum Progress
{
    Pending,
    Complete(Completion),
    Finished(Outcome)
}
/// Owned result outside Desktop. If commit refuses, Drop puts the SAME reply
/// back in its original job; no author call is restarted.
#[must_use = "commit or return the same reply outside Desktop"]
pub(crate) struct Completion
{
    job:      Arc<WindowJob>,
    reply:    Option<OperationReply>,
    accepted: bool
}
impl Completion
{
    pub(crate) fn window_id(&self) -> WindowId
    {
        self.job.plan().seal.id
    }

    pub(crate) fn storage_owner(&self) -> mac_object_table::domain::DomainId
    {
        self.job.storage_owner()
    }

    pub(crate) fn result(&self) -> Result<i64, Status>
    {
        self.reply.as_ref().map(OperationReply::result).ok_or(BAD_ABI)
    }

    pub(super) fn birth_reply(&self, job: &Arc<WindowJob>) -> Result<&Reply, Status>
    {
        if !Arc::ptr_eq(&self.job, job)
        {
            return Err(ACCESS_DENIED);
        }
        match self.reply.as_ref()
        {
            Some(OperationReply::User(reply)) => Ok(reply),
            _ => Err(BAD_ABI)
        }
    }

    pub(super) fn accept_birth(&mut self, job: &Arc<WindowJob>) -> Result<(), Status>
    {
        self.birth_reply(job)?;
        self.accept();
        Ok(())
    }

    fn accept(&mut self)
    {
        self.accepted = true;
        self.job.with_state(|state| state.consumed = true);
    }
}
impl Drop for Completion
{
    fn drop(&mut self)
    {
        let wake = self.job.with_state(|state| {
                               if !self.accepted
                               {
                                   // Checkout has exclusive custody of the one original reply.
                                   state.reply = self.reply.take();
                               }
                               state.busy = false;
                               core::mem::take(&mut state.waiting) && !state.commit_blocked
                           });
        if wake
        {
            self.job.available.request();
            self.job.wake_waiter();
        }
        // An accepted owned Reply (and its copied bytes) drops outside the gate.
    }
}
impl WindowJob
{
    pub(super) fn canvas_return_notice(&self) -> Arc<Notice>
    {
        self.notice.clone()
    }

    pub(crate) fn prepare(plan: OperationPlan, waiter: ThreadId, notice: Arc<Notice>)
                          -> Result<Arc<Self>, Status>
    {
        Self::prepare_inner(plan, Some(waiter), notice)
    }

    /// Prepaid with the actual native permit before author NEW. Its inactive
    /// copied plan is replaced only from the original releasing WIND payload.
    pub(super) fn prepare_cleanup(mut window: Window, id: WindowId, storage: &OwnerRef,
                                  notice: Arc<Notice>)
                                  -> Result<Arc<Self>, Status>
    {
        window.id = id;
        let owner = storage.owner();
        let plan = OperationPlan { seal: Seal { id,
                                                owner,
                                                revision: 1,
                                                entry: window.entry },
                                   window,
                                   purpose: Purpose::Dispose,
                                   tracking: None,
                                   tracking_revision: None };
        let job = Self::prepare_inner(plan, None, notice)?;
        let prepared = PreparedWindowCallback::prepare(window.entry,
                                                       window.procedure >> 4,
                                                       window.procedure & 15,
                                                       definition::DISPOSE,
                                                       window.state,
                                                       CompletionTarget::System(job.available
                                                                                   .clone()))?;
        job.with_state(|state| {
               state.prepared = Some(prepared);
               state.acknowledgement = Some(super::ack::Receipt::new(storage.clone()));
           });
        Ok(job)
    }

    pub(super) fn prepare_birth(window: Window, id: WindowId,
                                owner: mac_object_table::domain::DomainId, notice: Arc<Notice>,
                                waiter: Option<ThreadId>, message: i16)
                                -> Result<Arc<Self>, Status>
    {
        if !matches!(message, definition::NEW | definition::CALCULATE)
        {
            return Err(BAD_ARGUMENT);
        }
        let plan = OperationPlan { seal: Seal { id,
                                                owner,
                                                revision: 1,
                                                entry: window.entry },
                                   window,
                                   purpose: Purpose::Initialize(message),
                                   tracking: None,
                                   tracking_revision: None };
        let job = Self::prepare_inner(plan, waiter, notice)?;
        let target = match waiter
        {
            Some(thread) =>
            {
                CompletionTarget::ThreadAndSystem { thread,
                                                    notice: job.available.clone() }
            }
            None => CompletionTarget::System(job.available.clone())
        };
        let prepared = PreparedWindowCallback::prepare(window.entry,
                                                       window.procedure >> 4,
                                                       window.procedure & 15,
                                                       message,
                                                       window.state,
                                                       target)?;
        job.with_state(|state| state.prepared = Some(prepared));
        Ok(job)
    }

    pub(super) fn begin_prepared(&self, window: Window, id: WindowId,
                                 owner: mac_object_table::domain::DomainId)
                                 -> Result<(), Status>
    {
        let mut plan = self.plan();
        if plan.seal.id != id
           || plan.seal.owner != owner
           || !plan.seal.entry.same(window.entry)
           || !matches!(plan.purpose, Purpose::Initialize(_) | Purpose::Dispose)
        {
            return Err(ACCESS_DENIED);
        }
        plan.window = window;
        self.with_state(|state| {
                if state.admitted
                {
                    return Ok(());
                }
                if state.prepared.is_none() || state.started || state.busy
                {
                    return Err(BAD_ABI);
                }
                *self.plan.lock() = plan;
                state.admitted = true;
                Ok(())
            })
    }

    pub(super) fn author_admitted(&self) -> bool
    {
        self.with_state(|state| state.pending.is_some())
    }

    pub(super) fn discard_unused(&self)
    {
        let unused = self.with_state(|state| state.prepared.take());
        drop(unused);
    }

    fn prepare_inner(plan: OperationPlan, waiter: Option<ThreadId>, notice: Arc<Notice>)
                     -> Result<Arc<Self>, Status>
    {
        let available = Notice::linked(notice.clone())?;
        Arc::try_new(Self { canvas: super::canvas::CanvasState::new(),
                            plan: Spinlock::new(plan),
                            waiter,
                            notice,
                            available,
                            wake_pending: AtomicBool::new(false),
                            terminal_ready: AtomicBool::new(false),
                            needs_acknowledgement: AtomicBool::new(false),
                            acknowledged: AtomicBool::new(false),
                            quiescence_waiting: AtomicBool::new(false),
                            ui_blocked: AtomicBool::new(false),
                            ui_claimed: AtomicBool::new(false),
                            state: Spinlock::new(JobState { pending:  None, prepared: None,
                                                            reply:    None,
                                                            failure:  None,
                                                            started:  false,
                                                            busy:     false,
                                                            waiting:  false,
                                                            consumed: false,
                                                            admitted: false,
                                                            outcome: None,
                                                            cleanup: CleanupPhase::BeforeReply,
                                                            cleanup_reason: None,
                                                            cleanup_waiting: false,
                                                            cleanup_event: false,
                                                            quiescence_plan: None,
                                                            quiescence_proof: None,
                                                            quiescence_loaned: false,
                                                            quiescence_contender: false,
                                                            commit_blocked: false,
                                                            allocation_wait: false,
                                                            activation_armed: false,
                                                            activation_returned: false,
                                                            paint_exclusion: None,
                                                            acknowledgement: None }) }).map_err(|_| {
                                                                                     NO_MEMORY
                                                                                 })
    }

    fn plan(&self) -> OperationPlan
    {
        InvocationGate::run(|| *self.plan.lock())
    }

    pub(crate) fn window_id(&self) -> WindowId
    {
        self.plan().seal.id
    }

    pub(crate) fn storage_owner(&self) -> mac_object_table::domain::DomainId
    {
        self.plan().seal.owner
    }

    pub(crate) fn owner(&self) -> u64
    {
        self.plan().window.owner
    }

    pub(crate) fn waiter(&self) -> Option<ThreadId>
    {
        self.waiter
    }

    /// Pure Acquire hint for the authenticated scheduler park handshake. Never
    /// an acknowledgement/authority, and never acquires a Window/job mutex.
    pub(crate) fn wait_ready(&self) -> bool
    {
        if self.terminal_ready.load(Ordering::Acquire)
           && self.needs_acknowledgement.load(Ordering::Acquire)
           && !self.acknowledged.load(Ordering::Acquire)
        {
            return false;
        }
        !self.ui_blocked.load(Ordering::Acquire)
        && !self.ui_claimed.load(Ordering::Acquire)
        && (self.terminal_ready.load(Ordering::Acquire) || self.available.ready())
    }

    /// Run outside Desktop/SCHED after consuming actual manager/resource work.
    pub(crate) fn deliver_wake(&self)
    {
        if self.wake_pending.swap(false, Ordering::AcqRel)
        {
            self.wake_waiter();
        }
    }

    /// Root calls before actual original-ticket control-removal preflight.
    pub(crate) fn arm_removal_wait(&self)
    {
        self.arm_commit_wait();
    }

    /// Call only after that genuine same-Desktop admission succeeds. No wake
    /// is manufactured; the caller now performs its already-owned next step.
    pub(crate) fn removal_admitted(&self)
    {
        self.with_state(|state| {
                state.commit_blocked = false;
                state.allocation_wait = false;
            });
    }

    fn arm_commit_wait(&self)
    {
        self.with_state(|state| {
                state.commit_blocked = true;
                state.allocation_wait = false;
            });
        // Clear BEFORE the canonical attempt; later real return edges survive.
        self.available.take();
    }

    pub(super) fn resource_changed(&self) -> bool
    {
        let changed = self.with_state(|state| {
                              if state.activation_armed
                              {
                                  // Keep a real return which races the owned
                                  // activation attempt. Classification happens
                                  // after activate returns, outside this gate.
                                  state.activation_returned = true;
                                  return false;
                              }
                              if !state.commit_blocked || state.allocation_wait
                              {
                                  return false;
                              }
                              state.commit_blocked = false;
                              if !state.started
                                 && state.prepared.is_some()
                                 && state.failure == Some(SERVICE_BUSY)
                              {
                                  state.failure = None;
                              }
                              true
                          });
        if changed
        {
            self.wake_pending.store(true, Ordering::Release);
            self.available.request();
        }
        changed
    }

    fn cleanup_changed(&self) -> bool
    {
        let changed = self.with_state(|state| {
                              if state.cleanup == CleanupPhase::Claimed
                                 && self.ui_blocked.load(Ordering::Acquire)
                              {
                                  state.cleanup_event = true;
                                  return false;
                              }
                              if state.cleanup != CleanupPhase::Blocked
                              {
                                  return false;
                              }
                              state.cleanup = CleanupPhase::Pending;
                              true
                          });
        if changed
        {
            self.ui_blocked.store(false, Ordering::Release);
            self.wake_pending.store(true, Ordering::Release);
            self.available.request();
        }
        changed
    }

    fn wake_waiter(&self)
    {
        if let Some(waiter) = self.waiter
        {
            crate::machine::scheduler::callbacks::wake_waiter(waiter);
        }
    }

    fn activate_cleanup(&self, plan: OperationPlan) -> Result<(), Status>
    {
        if self.waiter.is_some()
        {
            return Err(ACCESS_DENIED);
        }
        self.with_state(|state| {
                if state.admitted || state.started || state.busy
                {
                    return Err(BAD_ABI);
                }
                *self.plan.lock() = plan;
                state.admitted = true;
                Ok(())
            })
    }

    fn with_state<R>(&self, operation: impl FnOnce(&mut JobState) -> R) -> R
    {
        InvocationGate::run(|| operation(&mut self.state.lock()))
    }

    /// Pure local readiness; no window enumeration or author call.
    pub(crate) fn ready(&self) -> bool
    {
        self.with_state(|state| {
                if state.outcome.is_some()
                {
                    return !self.needs_acknowledgement.load(Ordering::Acquire)
                           || self.acknowledged.load(Ordering::Acquire);
                }
                if state.busy
                {
                    state.waiting = true;
                    return false;
                }
                state.admitted
                && !state.commit_blocked
                && (state.outcome.is_some()
                    || (!state.consumed
                        && (state.reply.is_some()
                            || state.failure.is_some()
                            || !state.started
                            || state.pending.as_ref().is_some_and(Pending::ready))))
            })
    }

    pub(crate) fn advance(self: &Arc<Self>) -> Result<Progress, Status>
    {
        self.available.take();
        enum Turn
        {
            Finished(Outcome),
            Acknowledgement,
            Reply(OperationReply),
            Poll
            {
                pending:  Option<Pending>,
                prepared: Option<PreparedWindowCallback>,
                first:    bool
            }
        }
        let turn = self.with_state(|state| {
                           if let Some(outcome) = state.outcome
                           {
                               return Ok(if self.needs_acknowledgement.load(Ordering::Acquire)
                                            && !self.acknowledged.load(Ordering::Acquire)
                                         {
                                             Turn::Acknowledgement
                                         }
                                         else
                                         {
                                             Turn::Finished(outcome)
                                         });
                           }
                           if state.busy || state.commit_blocked
                           {
                               state.waiting = true;
                               return Err(SERVICE_BUSY);
                           }
                           if state.consumed || !state.admitted
                           {
                               return Err(BAD_ARGUMENT);
                           }
                           if let Some(status) = state.failure
                           {
                               return Err(status);
                           }
                           state.busy = true;
                           Ok(if let Some(reply) = state.reply.take()
                              {
                                  Turn::Reply(reply)
                              }
                              else
                              {
                                  Turn::Poll { pending:  state.pending.take(),
                                               prepared: state.prepared.take(),
                                               first:    !state.started }
                              })
                       })?;
        let (mut pending, mut prepared, first) = match turn
        {
            Turn::Finished(outcome) => return Ok(Progress::Finished(outcome)),
            Turn::Acknowledgement => return Ok(Progress::Pending),
            Turn::Poll { pending,
                         prepared,
                         first } => (pending, prepared, first),
            Turn::Reply(reply) =>
            {
                return Ok(Progress::Complete(Completion { job:      self.clone(),
                                                          reply:    Some(reply),
                                                          accepted: false }));
            }
        };
        let plan = self.plan();
        let result = if first
                        && matches!(plan.purpose,
                                    Purpose::Geometry { execution: GeometryExecution::Trusted,
                                                        .. })
        {
            // Guard-free and covered by the original admitted GeometryClaim.
            // Window.call pins the SAME trusted definition and original source
            // scope. No callback queue, Pending or fabricated Reply is created.
            let mut window = plan.window;
            let result = window.call(definition::CALCULATE, 0, core::ptr::null_mut(), None);
            Ok(Some(OperationReply::Trusted { result,
                                              state: window.state }))
        }
        else if first
        {
            self.with_state(|state| {
                    state.commit_blocked = true;
                    state.allocation_wait = prepared.is_none();
                    state.activation_armed = prepared.is_some();
                    state.activation_returned = false;
                    // Arm/clear under the SAME short exclusion as the event
                    // latch, before actual guard-free activation.
                    self.available.take();
                });
            let target = match self.waiter
            {
                Some(thread) =>
                {
                    CompletionTarget::ThreadAndSystem { thread,
                                                        notice: self.available.clone() }
                }
                None => CompletionTarget::System(self.available.clone())
            };
            let plan = self.plan();
            let result = if let Some(carrier) = prepared.take()
            {
                match carrier.activate(plan.window.state, target)
                {
                    Ok(value) => Ok(value),
                    Err((status, carrier)) =>
                    {
                        prepared = Some(carrier);
                        Err(status)
                    }
                }
            }
            else if matches!(plan.purpose, Purpose::Dispose)
            {
                Err(BAD_ABI)
            }
            else
            {
                callbacks::enqueue(plan.seal.entry, plan.request(), target)
            };
            result.map(|value| {
                      pending = Some(value);
                      None
                  })
        }
        else
        {
            pending.as_ref()
                   .ok_or(BAD_ABI)
                   .and_then(Pending::poll)
                   .map(|reply| reply.map(OperationReply::User))
        };
        let (result, wake) =
            self.with_state(|state| {
                    let allocation_refused =
                        first && pending.is_none() && matches!(result, Err(NO_MEMORY));
                    let prepared_refused = first
                                           && pending.is_none()
                                           && prepared.is_some()
                                           && matches!(result, Err(SERVICE_BUSY));
                    let activation_returned = core::mem::take(&mut state.activation_returned);
                    state.activation_armed = false;
                    let retry_return = prepared_refused && activation_returned;
                    state.started = !(allocation_refused || prepared_refused);
                    if prepared_refused
                    {
                        state.commit_blocked = !retry_return;
                        state.allocation_wait = false;
                    }
                    else if !allocation_refused
                    {
                        state.commit_blocked = false;
                        state.allocation_wait = false;
                    }

                    state.pending = pending;
                    state.prepared = prepared;
                    state.busy = false;
                    let wake = (core::mem::take(&mut state.waiting) && !state.commit_blocked)
                               || retry_return;
                    let result = match result
                    {
                        Ok(Some(reply)) =>
                        {
                            state.busy = true;
                            Ok(Progress::Complete(Completion { job:      self.clone(),
                                                               reply:    Some(reply),
                                                               accepted: false }))
                        }
                        Ok(None) => Ok(Progress::Pending),
                        Err(status) =>
                        {
                            state.failure = (!retry_return).then_some(status);
                            Err(status)
                        }
                    };
                    (result, wake)
                });
        if wake
        {
            self.available.request();
            self.wake_waiter();
        }
        result
    }

    pub(crate) fn take_close_cleanup(self: &Arc<Self>) -> Option<CloseCleanup>
    {
        self.with_state(|state| {
                if state.cleanup != CleanupPhase::Pending
                {
                    if state.cleanup == CleanupPhase::Claimed
                    {
                        state.cleanup_waiting = true;
                    }
                    return None;
                }
                // SAME job retains the original typed author/quiescence reason.
                state.cleanup_reason?;
                state.cleanup = CleanupPhase::Claimed;
                self.ui_claimed.store(true, Ordering::Release);
                Some(CloseCleanup::User(UserCloseCleanup { job:      self.clone(),
                                                           returned: false }))
            })
    }

    fn author_replied(&self, result: i64)
    {
        self.with_state(|state| {
                if state.cleanup == CleanupPhase::BeforeReply
                {
                    state.cleanup_reason = Some(CleanupReason::Author(result));
                    state.cleanup = CleanupPhase::Pending;
                }
            });
    }

    fn terminal(&self, outcome: Outcome)
    {
        let changed = self.with_state(|state| {
                              if state.outcome.is_some()
                              {
                                  return false;
                              }
                              state.outcome = Some(outcome);
                              state.commit_blocked = false;
                              true
                          });
        if changed
        {
            self.needs_acknowledgement
                .store(matches!(outcome, Outcome::Closed(Ok(_)) | Outcome::Quiesced(_)),
                       Ordering::Release);
            self.terminal_ready.store(true, Ordering::Release);
            self.wake_pending.store(true, Ordering::Release);
            self.available.request();
        }
    }

    fn cleanup_done(&self) -> bool
    {
        self.with_state(|state| state.cleanup == CleanupPhase::Done)
    }
}

/// These are existing producer intents, not a wire interface. Root validates
/// native request ownership before asking the manager to seal one.
#[derive(Clone, Copy)]
pub(crate) enum Geometry
{
    Bounds(Rect),
    Standard(Option<Rect>)
}
pub(crate) enum HitProgress
{
    Done(Hit),
    User(OperationPlan)
}
pub(crate) enum TrackingProgress
{
    Ordinary,
    Cancelled(OperationPlan),
    User(OperationPlan),
    Close(OperationPlan)
}
impl WindowManager
{
    pub(super) fn preflight_geometry(&self, id: WindowId) -> Result<(), Status>
    {
        self.windows.get(id.0).ok_or(NOT_FOUND)?.canvas_guardian()?.preflight_geometry()
    }

    pub(crate) fn operation_notice(&self) -> Arc<Notice>
    {
        self.work_notice.clone()
    }

    pub(crate) fn take_work_pending(&self) -> bool
    {
        self.work_notice.take()
    }

    /// Called by Root after a genuine source THRD reclaim, not every input.
    pub(crate) fn source_cleanup_changed(&self)
    {
        let mut changed = false;
        for debt in &self.removals
        {
            let job = match debt
            {
                RemovalDebt::Pending { job,
                                       .. } => job.as_ref(),
                RemovalDebt::Completed(removed) =>
                {
                    if let Some(header) = &removed.trusted
                    {
                        changed |= header.changed();
                    }
                    removed.job.as_ref()
                }
                RemovalDebt::Acknowledgement { job,
                                               trusted,
                                               .. } =>
                {
                    if let Some(header) = trusted
                    {
                        header.refresh_ack();
                    }
                    if let Some(job) = job
                    {
                        job.refresh_acknowledgement();
                    }
                    job.as_ref()
                }
                RemovalDebt::Trusted { handle,
                                       .. } =>
                {
                    changed |= handle.changed();
                    None
                }
                RemovalDebt::Birth(owner) =>
                {
                    changed |= owner.source_changed();
                    None
                }
            };
            if let Some(job) = job
            {
                changed |= job.resource_changed();
                changed |= job.cleanup_changed();
                changed |= job.quiescence_waiting.swap(false, Ordering::AcqRel);
            }
        }
        if changed
        {
            self.work_notice.request();
        }
    }

    fn seal(&self, id: WindowId) -> Result<Seal, Status>
    {
        let (owner, owned) = self.windows
                                 .get_with_owner(id.0)
                                 .filter(|(_, owned)| owned.initialized)
                                 .ok_or(NOT_FOUND)?;
        if !owned.binding.matches(owned.entry)
        {
            return Err(BAD_ABI);
        }
        Ok(Seal { id,
                  owner,
                  revision: owned.revision.ok_or(BAD_ABI)?,
                  entry: owned.entry })
    }

    fn validate_seal(&self, seal: Seal) -> Result<(), Status>
    {
        let actual = self.seal(seal.id)?;
        if actual.owner != seal.owner
           || actual.revision != seal.revision
           || !actual.entry.same(seal.entry)
        {
            return Err(SERVICE_BUSY);
        }
        Ok(())
    }

    /// Claim the original live WIND revision after guard-free job allocation,
    /// before enqueue. A second job from the same copied plan cannot also win.
    pub(crate) fn admit_operation(&mut self, job: &Arc<WindowJob>) -> Result<(), Status>
    {
        if matches!(job.plan().purpose, Purpose::Dispose | Purpose::Initialize(_))
        {
            return Err(BAD_ARGUMENT);
        }
        if !Arc::ptr_eq(&job.notice, &self.work_notice)
        {
            return Err(ACCESS_DENIED);
        }
        self.validate_seal(job.plan().seal)?;
        if matches!(job.plan().purpose, Purpose::Geometry { .. })
        {
            self.exclude_paint(job.plan().seal.id)?;
        }
        if let Purpose::Hit { scene,
                              .. } = job.plan().purpose
           && self.hit_revision != Some(scene)
        {
            return Err(SERVICE_BUSY);
        }
        if job.plan().tracking.is_some()
           && (self.tracking != job.plan().tracking
               || self.tracking_revision != job.plan().tracking_revision)
        {
            return Err(SERVICE_BUSY);
        }
        let next = job.plan().seal.revision.checked_add(1).ok_or(BAD_ABI)?;
        job.with_state(|state| {
               if state.admitted || state.busy || state.started
               {
                   return Err(BAD_ARGUMENT);
               }
               let owned = self.windows.get_mut(job.plan().seal.id.0).ok_or(NOT_FOUND)?;
               if matches!(job.plan().purpose, Purpose::Geometry { .. })
               {
                   state.paint_exclusion = Some(owned.canvas_guardian()?.geometry()?);
               }
               owned.revision = Some(next);
               state.admitted = true;
               Ok(())
           })
    }

    fn validate_claim(&self, job: &WindowJob) -> Result<(), Status>
    {
        let mut seal = job.plan().seal;
        seal.revision = seal.revision.checked_add(1).ok_or(BAD_ABI)?;
        self.validate_seal(seal)?;
        if !Arc::ptr_eq(&job.notice, &self.work_notice)
        {
            return Err(ACCESS_DENIED);
        }
        job.with_state(|state| {
               if state.admitted && state.busy && !state.consumed
               {
                   Ok(())
               }
               else
               {
                   Err(BAD_ARGUMENT)
               }
           })
    }

    /// `below` is the private cursor returned by the previous admitted HIT.
    /// A single physical event keeps the same scene receipt across every miss.
    fn hit_from(&self, point: Point, scene: u64, below: usize) -> Result<HitProgress, Status>
    {
        if self.hit_revision != Some(scene)
        {
            return Err(SERVICE_BUSY);
        }
        for index in (0..below.min(self.len)).rev()
        {
            let id = self.order[index];
            let Some(window) = self.window(id)
            else
            {
                continue;
            };
            if window.hidden || !window.structure_pieces().iter().any(|rect| rect.contains(point))
            {
                continue;
            }
            if crate::toolbox::resources::definitions::is_user(window.entry)?
            {
                return Ok(HitProgress::User(OperationPlan { seal:              self.seal(id)?,
                                                            window:            *window,
                                                            purpose:
                                                                Purpose::Hit { point,
                                                                               scene,
                                                                               below: index },
                                                            tracking:          None,
                                                            tracking_revision: None }));
            }
            let mut probe = *window;
            let param = ((point.v as u16 as u32) << 16) | point.h as u16 as u32;
            let result = probe.call(definition::HIT, i64::from(param), core::ptr::null_mut(), None);
            if result < 0
            {
                return Err(i32::try_from(result).unwrap_or(BAD_ABI));
            }
            if let Some(part) = part(result)
            {
                return Ok(HitProgress::Done(Hit { part,
                                                  window: Some(id) }));
            }
        }
        Ok(HitProgress::Done(Hit { part:   WindowPart::Desk,
                                   window: None }))
    }

    pub(crate) fn plan_hit(&self, point: Point) -> Result<HitProgress, Status>
    {
        if point.v >= 0 && point.v < 25
        {
            return Ok(HitProgress::Done(Hit { part:   WindowPart::MenuBar,
                                              window: None }));
        }
        self.hit_from(point, self.hit_revision.ok_or(BAD_ABI)?, self.len)
    }

    pub(crate) fn finish_hit(&self, completion: &mut Completion) -> Result<HitProgress, Status>
    {
        let plan = completion.job.plan();
        let Purpose::Hit { point,
                           scene,
                           below } = plan.purpose
        else
        {
            return Err(BAD_ARGUMENT);
        };
        self.validate_claim(&completion.job)?;
        if self.hit_revision != Some(scene)
        {
            return Err(SERVICE_BUSY);
        }
        let result = completion.result()?;
        if result < 0
        {
            return Err(i32::try_from(result).unwrap_or(BAD_ABI));
        }
        let progress = if let Some(part) = part(result)
        {
            HitProgress::Done(Hit { part,
                                    window: Some(plan.seal.id) })
        }
        else
        {
            self.hit_from(point, scene, below)?
        };
        completion.accept();
        Ok(progress)
    }

    pub(crate) fn plan_geometry(&self, id: WindowId, intent: Geometry)
                                -> Result<Option<OperationPlan>, Status>
    {
        let seal = self.seal(id)?;
        let mut window = *self.window(id).ok_or(NOT_FOUND)?;
        let execution = if crate::toolbox::resources::definitions::is_user(window.entry)?
        {
            GeometryExecution::User
        }
        else
        {
            GeometryExecution::Trusted
        };
        if matches!(intent, Geometry::Standard(None))
        {
            return Ok(None);
        }
        let standard = match intent
        {
            Geometry::Bounds(bounds) =>
            {
                if bounds == window.bounds
                {
                    return Ok(None);
                }
                window.bounds = bounds;
                window.user_bounds = None;
                window.state.zoomed = 0;
                None
            }
            Geometry::Standard(bounds) =>
            {
                if bounds == window.standard_bounds
                {
                    return Ok(None);
                }
                if let Some(bounds) = bounds
                {
                    window.bounds = bounds;
                }
                Some(bounds)
            }
        };
        validate_bounds(&window)?;
        window.state.content = window.bounds;
        Ok(Some(OperationPlan { seal,
                                window,
                                purpose: Purpose::Geometry { action: Action::Resize(id),
                                                             activate: false,
                                                             standard,
                                                             execution },
                                tracking: None,
                                tracking_revision: None }))
    }

    pub(crate) fn plan_tracking_release(&self, point: Point) -> Result<TrackingProgress, Status>
    {
        let Some(tracking) = self.tracking
        else
        {
            return Ok(TrackingProgress::Ordinary);
        };
        let id = match tracking
        {
            Tracking::Drag { window,
                             .. }
            | Tracking::Grow { window,
                             .. }
            | Tracking::Zoom { window,
                             .. }
            | Tracking::Close { window,
                              .. } => window
        };
        let mut window = *self.window(id).ok_or(NOT_FOUND)?;
        self.tracking_revision.ok_or(BAD_ABI)?;
        let execution = if crate::toolbox::resources::definitions::is_user(window.entry)?
        {
            GeometryExecution::User
        }
        else
        {
            GeometryExecution::Trusted
        };
        let mut activate = false;
        let action = match tracking
        {
            Tracking::Drag { start,
                             bounds,
                             activate: select,
                             .. } =>
            {
                let Some(bounds) = drag_geometry(bounds, start, point, self.screen)
                else
                {
                    return Ok(TrackingProgress::Cancelled(OperationPlan { seal: self.seal(id)?, window,
                    purpose: Purpose::Geometry { action: Action::Drag(id), activate: false, standard: None, execution },
                    tracking: Some(tracking), tracking_revision: self.tracking_revision }));
                };
                window.bounds = bounds;
                window.user_bounds = None;
                window.state.zoomed = 0;
                activate = select;
                Action::Drag(id)
            }
            Tracking::Grow { start,
                             bounds,
                             .. } =>
            {
                window.bounds = grow_geometry(&window, bounds, start, point, self.screen);
                window.user_bounds = None;
                window.state.zoomed = 0;
                Action::Resize(id)
            }
            Tracking::Zoom { .. } =>
            {
                if !window.state.zoom_box.contains(point)
                {
                    return Ok(TrackingProgress::Cancelled(OperationPlan { seal: self.seal(id)?, window,
                    purpose: Purpose::Geometry { action: Action::Drag(id), activate: false, standard: None, execution },
                    tracking: Some(tracking), tracking_revision: self.tracking_revision }));
                }
                zoom_geometry(&mut window, self.screen);
                Action::Resize(id)
            }
            Tracking::Close { .. } =>
            {
                if matches!(execution, GeometryExecution::Trusted)
                {
                    return Ok(TrackingProgress::Ordinary);
                }
                if !window.close_box().contains(point) || window.close_deferred
                {
                    return Ok(TrackingProgress::Ordinary);
                }
                return Ok(TrackingProgress::Close(OperationPlan { seal: self.seal(id)?,
                                                                  window,
                                                                  purpose: Purpose::Dispose,
                                                                  tracking: Some(tracking),
                                                                  tracking_revision:
                                                                      self.tracking_revision }));
            }
        };
        validate_bounds(&window)?;
        if !matches!(tracking, Tracking::Zoom { .. })
           && self.window(id).is_some_and(|old| old.bounds == window.bounds)
        {
            return Ok(TrackingProgress::Ordinary);
        }
        window.state.content = window.bounds;
        Ok(TrackingProgress::User(OperationPlan { seal: self.seal(id)?,
                                                  window,
                                                  purpose: Purpose::Geometry { action,
                                                                               activate,
                                                                               standard:
                                                                                   None,
                                                                               execution },
                                                  tracking: Some(tracking),
                                                  tracking_revision: self.tracking_revision }))
    }

    /// Cancel only the original physical release's tracking receipt; a later
    /// tracking operation cannot be cleared by an obsolete completion.
    pub(crate) fn cancel_tracking_operation(&mut self, plan: &OperationPlan)
    {
        if plan.tracking.is_some()
           && self.tracking == plan.tracking
           && self.tracking_revision == plan.tracking_revision
        {
            self.tracking = None;
        }
    }

    /// Actual validated staged viewport for guard-free managed-control layout
    /// preparation. This query runs under Desktop; finish_geometry repeats the
    /// same claim/reply checks before installing the prepared control layout.
    pub(crate) fn proposed_viewport(&self, completion: &Completion)
                                    -> Result<Option<Rect>, Status>
    {
        let plan = completion.job.plan();
        let Purpose::Geometry { standard,
                                .. } = plan.purpose
        else
        {
            return Err(BAD_ARGUMENT);
        };
        self.validate_claim(&completion.job)?;
        self.exclude_paint(plan.seal.id)?;
        if plan.tracking.is_some()
           && (self.tracking != plan.tracking || self.tracking_revision != plan.tracking_revision)
        {
            return Err(SERVICE_BUSY);
        }
        let reply = completion.reply.as_ref().ok_or(BAD_ABI)?;
        if reply.result() < 0
        {
            return Err(i32::try_from(reply.result()).unwrap_or(BAD_ABI));
        }
        validate_state(&reply.window()?, &plan.window)?;
        if standard.is_some()
        {
            return Ok(None);
        }
        let current = self.window(plan.seal.id).ok_or(NOT_FOUND)?;
        let viewport = plan.window.local_bounds();
        Ok((current.local_bounds() != viewport).then_some(viewport))
    }

    pub(crate) fn finish_geometry(&mut self, completion: &mut Completion,
                                  mut prepare: impl FnMut(&Self, WindowId, Rect) -> Result<(), Status>,
                                  events: &mut EventManager, when: u32)
                                  -> Result<Option<Action>, Status>
    {
        let plan = completion.job.plan();
        let Purpose::Geometry { action,
                                activate,
                                standard,
                                .. } = plan.purpose
        else
        {
            return Err(BAD_ARGUMENT);
        };
        self.validate_claim(&completion.job)?;
        self.exclude_paint(plan.seal.id)?;
        if plan.tracking.is_some()
           && (self.tracking != plan.tracking || self.tracking_revision != plan.tracking_revision)
        {
            return Err(SERVICE_BUSY);
        }
        let reply = completion.reply.as_ref().ok_or(BAD_ABI)?;
        if reply.result() < 0
        {
            return Err(i32::try_from(reply.result()).unwrap_or(BAD_ABI));
        }
        let state = reply.window()?;
        validate_state(&state, &plan.window)?;
        if let Some(bounds) = standard
        {
            if bounds.is_some()
               && (state.structure.left < 0
                   || state.structure.top < 25
                   || state.structure.right > self.screen.right
                   || state.structure.bottom > self.screen.bottom - 21)
            {
                return Err(BAD_ARGUMENT);
            }
            self.window_mut(plan.seal.id).ok_or(NOT_FOUND)?.standard_bounds = bounds;
            completion.accept();
            return Ok(None);
        }
        let previous = *self.window(plan.seal.id).ok_or(NOT_FOUND)?;
        let mut staged = plan.window;
        staged.state = state;
        if previous.local_bounds() != staged.local_bounds()
        {
            prepare(self, plan.seal.id, staged.local_bounds())?;
        }
        *self.window_mut(plan.seal.id).ok_or(NOT_FOUND)? = staged;
        let mut order = core::mem::take(&mut self.geometry_order);
        order.clear();
        order.extend_from_slice(&self.order);
        if plan.tracking.is_some()
        {
            self.tracking = None;
        }
        if activate
        {
            self.select_window(plan.seal.id, events, when);
        }
        if plan.tracking.is_some()
        {
            self.invalidate_geometry_exposure(&previous, &order, action);
        }
        self.geometry_order = order;
        completion.accept();
        Ok(Some(action))
    }
}
fn part(result: i64) -> Option<WindowPart>
{
    match result
    {
        1 => Some(WindowPart::Content),
        2 => Some(WindowPart::Drag),
        3 => Some(WindowPart::Grow),
        4 => Some(WindowPart::GoAway),
        5 => Some(WindowPart::ZoomIn),
        6 => Some(WindowPart::ZoomOut),
        _ => None
    }
}
pub(super) fn validate_bounds(window: &Window) -> Result<(), Status>
{
    let width = i32::from(window.bounds.right) - i32::from(window.bounds.left);
    let height = i32::from(window.bounds.bottom) - i32::from(window.bounds.top);
    if width < i32::from(window.min_width)
       || height < i32::from(window.min_height)
       || width > i32::from(i16::MAX)
       || height > i32::from(i16::MAX)
    {
        return Err(BAD_ARGUMENT);
    }
    Ok(())
}
pub(super) fn validate_state(state: &WindowDefinitionState, _window: &Window)
                             -> Result<(), Status>
{
    if state.title_length as usize > state.title.len()
       || state.structure_piece_count > 2
       || core::str::from_utf8(&state.title[..state.title_length as usize]).is_err()
       || state.structure.is_empty()
    {
        return Err(BAD_ABI);
    }
    Ok(())
}

/// SAME original plan/proof across refusal. Neither a failed acquire nor a
/// refused Desktop entry drops Loading/Definition just to manufacture a wake.
#[must_use = "retain the original source cleanup claim across refusal"]
pub(crate) struct QuiescenceWork
{
    job:   Arc<WindowJob>,
    plan:  Option<QuiescencePlan>,
    proof: Option<QuiescedDefinition>
}
impl QuiescenceWork
{
    pub(crate) fn window_id(&self) -> WindowId
    {
        self.job.window_id()
    }

    pub(crate) fn acquire(&mut self) -> Result<(), Status>
    {
        if self.proof.is_some()
        {
            return Ok(());
        }
        let plan = self.plan.take().ok_or(BAD_ABI)?;
        match plan.acquire()
        {
            Ok(proof) =>
            {
                self.proof = Some(proof);
                let unused = self.job.with_state(|state| state.prepared.take());
                drop(unused);
                Ok(())
            }
            Err((status, plan)) =>
            {
                self.plan = Some(plan);
                self.job.quiescence_waiting.store(true, Ordering::Release);
                Err(status)
            }
        }
    }

    pub(crate) fn proof(&mut self) -> &mut Option<QuiescedDefinition>
    {
        &mut self.proof
    }

    pub(crate) fn defer(&self)
    {
        self.job.quiescence_waiting.store(true, Ordering::Release);
    }
}
impl Drop for QuiescenceWork
{
    fn drop(&mut self)
    {
        let waiting = self.job.with_state(|state| {
                                  state.quiescence_plan = self.plan.take();
                                  state.quiescence_proof = self.proof.take();
                                  state.quiescence_loaned = false;
                                  core::mem::take(&mut state.quiescence_contender)
                              });
        if waiting
        {
            self.job.notice.request();
        }
    }
}

pub(super) struct RemovalReaders
{
    count:  AtomicUsize,
    notice: Arc<Notice>
}
impl RemovalReaders
{
    pub(super) fn new(notice: Arc<Notice>) -> Self
    {
        Self { count: AtomicUsize::new(0),
               notice }
    }
}
/// Actual canonical completion guard, detached only for guard-free Drop. Its
/// count covers physical detached guards. An acknowledgement row retains the
/// original source proof/owner/key through any later canonical ACK refusal.
#[must_use = "drop the actual Removed/proof outside Desktop"]
pub(crate) struct Removal
{
    removed: Option<Removed<OwnedWindow, InvocationGate>>,
    proof:   Option<QuiescedDefinition>,
    job:     Option<Arc<WindowJob>>,
    trusted: Option<super::trusted::TrustedCloseHandle>,
    readers: Arc<RemovalReaders>
}
impl Removal
{
    pub(super) fn trusted_handle(&self) -> Option<super::trusted::TrustedCloseHandle>
    {
        self.trusted.clone()
    }

    pub(super) fn trusted_for(&self, id: WindowId) -> Option<super::trusted::TrustedCloseHandle>
    {
        self.trusted.as_ref().filter(|handle| handle.window_id() == id).cloned()
    }
}
impl Drop for Removal
{
    fn drop(&mut self)
    {
        drop(self.removed.take());
        if let Some(job) = &self.job
        {
            job.payload_destroyed();
        }
        if let Some(trusted) = &self.trusted
        {
            trusted.resource_released();
        }
        drop(self.trusted.take());
        drop(self.proof.take());
        drop(self.job.take());
        self.readers.count.fetch_sub(1, Ordering::AcqRel);
        self.readers.notice.request();
    }
}
pub(super) enum RemovalDebt
{
    Birth(Arc<super::birth::BirthOwner>),
    Pending
    {
        ticket: ReleasedResource,
        job:    Option<Arc<WindowJob>>
    },
    Trusted
    {
        ticket: ReleasedResource,
        handle: super::trusted::TrustedCloseHandle
    },
    Acknowledgement
    {
        proof:   Option<QuiescedDefinition>,
        job:     Option<Arc<WindowJob>>,
        trusted: Option<super::trusted::TrustedCloseHandle>
    },
    Completed(Removal)
}
impl RemovalDebt
{
    pub(super) fn completed_trusted(removed: Removed<OwnedWindow, InvocationGate>,
                                    readers: Arc<RemovalReaders>,
                                    trusted: super::trusted::TrustedCloseHandle)
                                    -> Self
    {
        readers.count.fetch_add(1, Ordering::AcqRel);
        Self::Completed(Removal { removed: Some(removed),
                                  proof: None,
                                  job: None,
                                  trusted: Some(trusted),
                                  readers })
    }

    pub(super) fn completed(removed: Removed<OwnedWindow, InvocationGate>,
                            readers: Arc<RemovalReaders>, proof: Option<QuiescedDefinition>)
                            -> Self
    {
        readers.count.fetch_add(1, Ordering::AcqRel);
        Self::Completed(Removal { removed: Some(removed),
                                  proof,
                                  job: None,
                                  trusted: None,
                                  readers })
    }
}
impl WindowManager
{
    pub(super) fn removal_capacity_required(&self, additional: usize)
                                            -> Result<usize, CreationError>
    {
        // Store iteration includes unpublished/releasing payloads. Detached
        // guards remain counted until AFTER their actual completion Drop.
        self.windows
            .retained_values()
            .count()
            .checked_add(self.removals
                             .iter()
                             .filter(|debt| matches!(debt, RemovalDebt::Birth(_)))
                             .count())
            .and_then(|count| {
                count.checked_add(self.removals
                                      .iter()
                                      .filter(|debt| {
                                          matches!(debt, RemovalDebt::Acknowledgement { .. })
                                      })
                                      .count())
            })
            .and_then(|count| count.checked_add(self.windows.publication_credits()))
            .and_then(|count| count.checked_add(self.removal_readers.count.load(Ordering::Acquire)))
            .and_then(|count| count.checked_add(additional))
            .ok_or(CreationError::NoMemory)
    }

    pub(crate) fn plan_user_close(&self, id: WindowId) -> Result<Option<OperationPlan>, Status>
    {
        let seal = self.seal(id)?;
        let window = *self.window(id).ok_or(NOT_FOUND)?;
        if !crate::toolbox::resources::definitions::is_user(window.entry)?
        {
            return Ok(None);
        }
        Ok(Some(OperationPlan { seal,
                                window,
                                purpose: Purpose::Dispose,
                                tracking: None,
                                tracking_revision: None }))
    }

    /// Original-ticket abort of the old synchronous User birth which never
    /// admitted NEW. This is a real WIND release, not author DISPOSE success.
    pub(super) fn abort_uninitialized_user(&mut self, ticket: ReleasedResource)
                                           -> Result<WindowId, Failure>
    {
        let Some(owned) = self.windows.releasing(&ticket)
        else
        {
            return Err(Failure::registration_mismatch(ticket));
        };
        if owned.initialized || owned.user_initialization != UserInitialization::None
        {
            return Err(Failure::registration_mismatch(ticket));
        }
        let id = owned.id;
        if self.removals.len() == self.removals.capacity()
        {
            return Err(Failure::window_retirement_storage(ticket,
                                                          self.removals.len(),
                                                          self.removals.capacity()));
        }
        let removed = self.windows.release(ticket).map_err(Failure::from)?;
        self.removals
            .push_back(RemovalDebt::completed(removed, self.removal_readers.clone(), None));
        if let Some(held) = self.recovery_windows.iter_mut().flatten().find(|held| held.id == id)
        {
            held.disposed = true;
        }
        self.work_notice.request();
        Ok(id)
    }

    /// Transfer an already-invalidated User WIND ticket from its canonical
    /// owner walk. This never invokes a stopped author and never creates a job.
    /// Success means WM owns the ticket, not that WIND has acknowledged release.
    pub(super) fn defer_user_release(&mut self, ticket: ReleasedResource)
                                     -> Result<WindowId, Failure>
    {
        let Some(owned) = self.windows.releasing(&ticket)
        else
        {
            return Err(Failure::registration_mismatch(ticket));
        };
        let window = owned.window;
        if !owned.binding.matches(window.entry)
        {
            return Err(Failure::window_definition(ticket,
                                                  window.id.0,
                                                  window.procedure,
                                                  i64::from(BAD_ABI)));
        }
        if self.removals.len() == self.removals.capacity()
        {
            return Err(Failure::window_retirement_storage(ticket,
                                                          self.removals.len(),
                                                          self.removals.capacity()));
        }
        let Some(job) = owned.cleanup_job.as_ref().cloned()
        else
        {
            return Err(Failure::window_definition(ticket,
                                                  window.id.0,
                                                  window.procedure,
                                                  i64::from(BAD_ABI)));
        };
        let Some(revision) = owned.revision
        else
        {
            return Err(Failure::window_definition(ticket,
                                                  window.id.0,
                                                  window.procedure,
                                                  i64::from(BAD_ABI)));
        };
        let mut staged = window;
        if let Some(state) = owned.disposal_state.get()
        {
            staged.state = state;
        }
        let plan = OperationPlan { seal:              Seal { id: window.id,
                                                             owner: ticket.key().owner(),
                                                             revision,
                                                             entry: window.entry },
                                   window:            staged,
                                   purpose:           Purpose::Dispose,
                                   tracking:          None,
                                   tracking_revision: None };
        if let Err(status) = job.activate_cleanup(plan)
        {
            return Err(Failure::window_definition(ticket,
                                                  window.id.0,
                                                  window.procedure,
                                                  i64::from(status)));
        }
        job.with_state(|state| {
               if let Some(ack) = &mut state.acknowledgement
               {
                   ack.bind(ticket.key());
               }
           });
        self.retire_window_ui(&window);
        self.removals.push_back(RemovalDebt::Pending { ticket,
                                                       job: Some(job) });
        self.work_notice.request();
        Ok(window.id)
    }

    /// Call only after actual detached Removed guards have dropped outside
    /// Desktop. This checks the existing real completion tokens; no lookup or
    /// semantic close substitutes for their ACK.
    pub(crate) fn removed_dropped(&mut self)
    {
        self.source_cleanup_changed();
        self.finish_recovery_acknowledgements();
    }

    /// All fallible admission precedes invalidation. Root first prepares drag
    /// close, then calls this exactly once and runs its normal close UI suffix.
    pub(crate) fn admit_user_close(&mut self, job: &Arc<WindowJob>, events: &mut EventManager,
                                   when: u32)
                                   -> Result<WindowId, Status>
    {
        if !matches!(job.plan().purpose, Purpose::Dispose)
        {
            return Err(BAD_ARGUMENT);
        }
        self.validate_seal(job.plan().seal)?;
        self.exclude_paint(job.plan().seal.id)?;
        if job.plan().tracking.is_some()
           && (self.tracking != job.plan().tracking
               || self.tracking_revision != job.plan().tracking_revision)
        {
            return Err(SERVICE_BUSY);
        }
        if !Arc::ptr_eq(&job.notice, &self.work_notice)
        {
            return Err(ACCESS_DENIED);
        }
        if self.removals.len() == self.removals.capacity()
        {
            return Err(NO_MEMORY);
        }
        let id = job.plan().seal.id;
        let active = self.active == Some(id);
        let donor = self.windows
                        .get(id.0)
                        .and_then(|owned| owned.cleanup_job.as_ref())
                        .cloned()
                        .ok_or(BAD_ABI)?;
        if Arc::ptr_eq(&donor, job)
        {
            return Err(BAD_ARGUMENT);
        }
        let ticket =
            donor.with_state(|stock| {
                     job.with_state(|destination| {
                            if stock.acknowledgement
                                    .as_ref()
                                    .is_none_or(|ack| ack.owner().owner() != job.plan().seal.owner)
                               || destination.acknowledgement.is_some()
                               || stock.prepared.is_none()
                               || stock.admitted
                               || stock.started
                               || stock.busy
                               || destination.prepared.is_some()
                               || destination.admitted
                               || destination.started
                               || destination.busy
                            {
                                return Err(BAD_ABI);
                            }
                            donor.canvas.preflight_close()?;
                            let ticket = self.windows.begin_release(id.0).ok_or(NOT_FOUND)?;
                            donor.canvas.closed();
                            destination.prepared = stock.prepared.take();
                            destination.acknowledgement = stock.acknowledgement.take();
                            if let Some(ack) = &mut destination.acknowledgement
                            {
                                ack.bind(ticket.key());
                            }
                            Ok(ticket)
                        })
                 })?;
        self.retire_window_ui(&job.plan().window);
        job.with_state(|state| state.admitted = true);
        self.removals.push_back(RemovalDebt::Pending { ticket,
                                                       job: Some(job.clone()) });
        if active
        {
            self.active = self.front_window();
            self.refresh_active(None);
            Self::activation(self.active, true, events, when);
        }
        self.work_notice.request();
        Ok(id)
    }

    /// Same original releasing payload, validated under Desktop before another
    /// manager's real close admission (e.g. pending managed-control claims).
    /// The returned Copy geometry is metadata, not a new mutation capability.
    pub(crate) fn removal_window(&self, job: &Arc<WindowJob>) -> Result<Window, Status>
    {
        let plan = job.plan();
        let ticket = self.removals
                         .iter()
                         .find_map(|debt| {
                             match debt
                             {
                                 RemovalDebt::Pending { ticket,
                                                        job: Some(actual) }
                                     if Arc::ptr_eq(actual, job) =>
                                 {
                                     Some(ticket)
                                 }
                                 _ => None
                             }
                         })
                         .ok_or(NOT_FOUND)?;
        let owned = self.windows.releasing(ticket).ok_or(NOT_FOUND)?;
        if owned.id != plan.seal.id
           || ticket.key().owner() != plan.seal.owner
           || owned.revision != Some(plan.seal.revision)
           || !owned.binding.matches(plan.seal.entry)
        {
            return Err(ACCESS_DENIED);
        }
        Ok(owned.window)
    }

    /// Root's real work consumer traverses only this admitted debt directory
    /// after a notice, never all windows per input/Idle. A retained Pending is
    /// the same actual broker work, including after original Context teardown.
    pub(crate) fn removal_work(&self, index: usize) -> Option<Arc<WindowJob>>
    {
        match self.removals.get(index)?
        {
            RemovalDebt::Pending { job,
                                   .. } => job.clone(),
            RemovalDebt::Completed(removed) => removed.job.clone(),
            RemovalDebt::Acknowledgement { job,
                                           .. } => job.clone(),
            RemovalDebt::Birth(_) | RemovalDebt::Trusted { .. } => None
        }
    }

    pub(crate) fn birth_work(&self, index: usize) -> Option<Arc<super::birth::BirthOwner>>
    {
        match self.removals.get(index)?
        {
            RemovalDebt::Birth(owner) if owner.needs_cleanup() => Some(owner.clone()),
            _ => None
        }
    }

    pub(crate) fn take_retired_birth(&mut self) -> Option<super::birth::RetiredBirth>
    {
        let index =
            self.removals
                .iter()
                .position(|debt| matches!(debt, RemovalDebt::Birth(owner) if owner.finished()))?;
        match self.removals.remove(index)?
        {
            RemovalDebt::Birth(owner) => Some(super::birth::RetiredBirth { _owner: owner }),
            other =>
            {
                self.removals.insert(index, other);
                None
            }
        }
    }

    pub(crate) fn removal_work_len(&self) -> usize
    {
        self.removals.len()
    }

    pub(crate) fn take_removed(&mut self) -> Option<Removal>
    {
        let index = self.removals.iter().position(|debt| {
                                             matches!(debt, RemovalDebt::Completed(removed)
                if removed.job.as_ref().is_none_or(|job| job.cleanup_done())
                   && removed.trusted.as_ref().is_none_or(|header| header.ui_done()))
                                         })?;
        match self.removals.remove(index)?
        {
            RemovalDebt::Completed(mut value) =>
            {
                if value.job.is_some() || value.trusted.is_some() || value.proof.is_some()
                {
                    self.removals.insert(index,
                                         RemovalDebt::Acknowledgement { proof:   value.proof.take(),
                                                                        job:     value.job.clone(),
                                                                        trusted: value.trusted
                                                                                      .clone() });
                }
                Some(value)
            }
            other =>
            {
                self.removals.insert(index, other);
                None
            }
        }
    }

    pub(crate) fn finish_user_close(&mut self, completion: &mut Completion)
                                    -> Result<WindowId, CloseError>
    {
        let id = completion.window_id();
        let index = self.removals
                        .iter()
                        .position(|debt| {
                            matches!(debt,
            RemovalDebt::Pending { job: Some(job), .. } if Arc::ptr_eq(job, &completion.job))
                        })
                        .ok_or(CloseError::NotFound(id))?;
        let RemovalDebt::Pending { ticket,
                                   job: Some(job) } = &self.removals[index]
        else
        {
            return Err(CloseError::NotFound(id));
        };
        let owned = self.windows.releasing(ticket).ok_or(CloseError::NotFound(id))?;
        if !owned.binding.matches(job.plan().seal.entry)
           || owned.revision != Some(job.plan().seal.revision)
        {
            return Err(CloseError::NotFound(id));
        }
        let reply = completion.reply.as_ref().ok_or(CloseError::NotFound(id))?;
        let state = reply.window().map_err(|status| {
                                       CloseError::Cleanup(
            crate::toolbox::handles::cleanup::Cause::WindowDefinitionDisposal {
                window: id.0, procedure: owned.procedure, reply: i64::from(status) })
                                   })?;
        owned.disposal_state.set(Some(state));
        completion.job.author_replied(reply.result());
        if reply.result() < 0
        {
            let cause = crate::toolbox::handles::cleanup::Cause::WindowDefinitionDisposal {
                window: id.0, procedure: owned.procedure, reply: reply.result() };
            completion.job.terminal(Outcome::Closed(Err(CloseError::Cleanup(cause))));
            completion.accept();
            return Err(CloseError::Cleanup(cause));
        }
        completion.job.arm_commit_wait();
        self.complete_removal(index, None)?;
        completion.job.terminal(Outcome::Closed(Ok(id)));
        completion.accept();
        Ok(id)
    }

    /// Checkout the actual original bound plan/proof under Desktop; acquire
    /// and Drop/restore run outside. No failed plan is discarded/recreated.
    pub(crate) fn quiescence_work(&self, job: &Arc<WindowJob>) -> Result<QuiescenceWork, Status>
    {
        self.removal_window(job)?;
        let ticket = self.removals
                         .iter()
                         .find_map(|debt| {
                             match debt
                             {
                                 RemovalDebt::Pending { ticket,
                                                        job: Some(actual) }
                                     if Arc::ptr_eq(actual, job) =>
                                 {
                                     Some(ticket)
                                 }
                                 _ => None
                             }
                         })
                         .ok_or(NOT_FOUND)?;
        let owned = self.windows.releasing(ticket).ok_or(NOT_FOUND)?;
        job.with_state(|state| {
               if state.quiescence_loaned
               {
                   state.quiescence_contender = true;
                   return Err(SERVICE_BUSY);
               }
               if state.quiescence_plan.is_none() && state.quiescence_proof.is_none()
               {
                   state.quiescence_plan = Some(owned.binding.quiescence_plan());
               }
               state.quiescence_loaned = true;
               Ok(QuiescenceWork { job:   job.clone(),
                                   plan:  state.quiescence_plan.take(),
                                   proof: state.quiescence_proof.take() })
           })
    }

    pub(crate) fn finish_quiesced_close(&mut self, id: WindowId,
                                        proof: &mut Option<QuiescedDefinition>)
                                        -> Result<WindowId, CloseError>
    {
        let index = self.removals.iter().position(|debt| matches!(debt,
            RemovalDebt::Pending { ticket, .. } if self.windows.releasing(ticket).is_some_and(|owned| owned.id == id)))
            .ok_or(CloseError::NotFound(id))?;
        let RemovalDebt::Pending { ticket,
                                   .. } = &self.removals[index]
        else
        {
            return Err(CloseError::NotFound(id));
        };
        let owned = self.windows.releasing(ticket).ok_or(CloseError::NotFound(id))?;
        if proof.as_ref().is_none_or(|proof| !proof.matches(&owned.binding))
        {
            return Err(CloseError::NotFound(id));
        }
        self.complete_removal(index, Some(proof))?;
        Ok(id)
    }

    fn complete_removal(&mut self, index: usize,
                        mut proof: Option<&mut Option<QuiescedDefinition>>)
                        -> Result<(), CloseError>
    {
        let id = match self.removals.get(index)
        {
            Some(RemovalDebt::Pending { ticket,
                                        .. }) =>
            {
                self.windows
                    .releasing(ticket)
                    .map(|owned| owned.id)
                    .ok_or(CloseError::NotFound(WindowId(0)))?
            }
            _ => return Err(CloseError::NotFound(WindowId(0)))
        };
        let (ticket, job) = match self.removals.remove(index)
        {
            Some(RemovalDebt::Pending { ticket,
                                        job }) => (ticket, job),
            Some(other) =>
            {
                self.removals.insert(index, other);
                return Err(CloseError::NotFound(WindowId(0)));
            }
            None => return Err(CloseError::NotFound(WindowId(0)))
        };
        let removed = match self.windows.release(ticket)
        {
            Ok(value) => value,
            Err(error) =>
            {
                let (ticket, cause) = Failure::from(error).into_parts();
                self.removals.insert(index,
                                     RemovalDebt::Pending { ticket,
                                                            job });
                return Err(CloseError::Cleanup(cause));
            }
        };
        if proof.is_some()
           && let Some(job) = &job
        {
            job.with_state(|state| {
                   if state.cleanup == CleanupPhase::BeforeReply
                   {
                       state.cleanup_reason = Some(CleanupReason::Quiesced);
                       state.cleanup = CleanupPhase::Pending;
                   }
               });
            job.terminal(Outcome::Quiesced(id));
        }
        self.removal_readers.count.fetch_add(1, Ordering::AcqRel);
        self.removals.insert(index,
                             RemovalDebt::Completed(Removal { removed: Some(removed),
                                                              proof: proof.as_mut()
                                                                          .and_then(|proof| {
                                                                              proof.take()
                                                                          }),
                                                              job,
                                                              trusted: None,
                                                              readers: self.removal_readers
                                                                           .clone() }));
        #[cfg(target_arch = "riscv64")]
        if let Some(held) = self.recovery_windows.iter_mut().flatten().find(|held| held.id == id)
        {
            held.disposed = true;
        }
        self.work_notice.request();
        Ok(())
    }
}

impl WindowJob
{
    /// Observe the SAME admitted close result. This neither polls a callback,
    /// refreshes a ledger, consumes a reply nor authorizes another operation.
    /// Success/quiescence requires actual original WIND ACK; a real author
    /// error remains observable after UI cleanup with its ticket retained.
    pub(crate) fn close_result(&self) -> Option<Outcome>
    {
        self.with_state(|state| {
                let outcome = state.outcome?;
                if state.cleanup != CleanupPhase::Done
                {
                    return None;
                }
                if matches!(outcome, Outcome::Closed(Ok(_)) | Outcome::Quiesced(_))
                   && !self.acknowledged.load(Ordering::Acquire)
                {
                    return None;
                }
                Some(outcome)
            })
    }

    fn payload_destroyed(&self)
    {
        self.with_state(|state| {
                if let Some(receipt) = &mut state.acknowledgement
                {
                    receipt.destroyed();
                }
            });
        self.refresh_acknowledgement();
    }

    fn refresh_acknowledgement(&self) -> bool
    {
        let complete =
            self.with_state(|state| {
                    state.acknowledgement.as_mut().is_some_and(|receipt| receipt.refresh())
                });
        if complete && !self.acknowledged.swap(true, Ordering::AcqRel)
        {
            self.wake_pending.store(true, Ordering::Release);
            self.available.request();
        }
        complete
    }
}
/// The SAME source-quiescence proof and callback metadata remain admitted
/// through actual authoritative WIND ACK, then leave Desktop for destruction.
#[must_use = "drop original acknowledgement custody outside Desktop"]
pub(crate) struct RetiredAcknowledgement
{
    // Drop order is protocol order: release original source proof, then actual
    // callback/header readers. The carrier itself leaves Desktop before Drop.
    _proof:   Option<QuiescedDefinition>,
    _job:     Option<Arc<WindowJob>>,
    _trusted: Option<super::trusted::TrustedCloseHandle>
}
impl WindowManager
{
    pub(crate) fn take_retired_acknowledgement(&mut self) -> Option<RetiredAcknowledgement>
    {
        let index = self.removals.iter().position(|debt| {
                                             match debt
                                     {
                                         RemovalDebt::Acknowledgement { job,
                                                                        trusted,
                                                                        .. } =>
                                         {
                                             (job.is_some() || trusted.is_some())
                                             && job.as_ref().is_none_or(|job| {
                                                                job.acknowledged
                                                                   .load(Ordering::Acquire)
                                                                && job.cleanup_done()
                                                            })
                                             && trusted.as_ref().is_none_or(|header| {
                                                                    header.acknowledgement_done()
                                                                    && header.ui_done()
                                                                })
                                         }
                                         _ => false
                                     }
                                         })?;
        match self.removals.remove(index)?
        {
            RemovalDebt::Acknowledgement { proof,
                                           job,
                                           trusted } =>
            {
                Some(RetiredAcknowledgement { _proof:   proof,
                                              _job:     job,
                                              _trusted: trusted })
            }
            other =>
            {
                self.removals.insert(index, other);
                None
            }
        }
    }
}
