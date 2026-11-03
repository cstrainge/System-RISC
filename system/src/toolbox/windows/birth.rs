//! Canonical custody for an unpublished native WIND after author NEW.
//! Every callback/permit is prepaid before registration; cancellation leaves
//! this owner in the manager directory until disposal or genuine quiescence.
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::{ACCESS_DENIED, BAD_ABI, SERVICE_BUSY, Status};
use spinning_top::Spinlock;

use super::{admission::PreparedWindow,
            operations::{Progress, WindowJob},
            *};
use crate::{context::ThreadId,
            toolbox::resources::definitions::{QuiescedDefinition, QuiescencePlan,
                                              callbacks::Notice}};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase
{
    New,
    Calculate,
    Ready,
    Dispose,
    Refused,
    Published,
    Done
}
struct State
{
    prepared:      Option<PreparedWindow>,
    phase:         Phase,
    busy:          bool,
    contender:     bool,
    failure:       Option<Status>,
    plan:          Option<QuiescencePlan>,
    proof:         Option<QuiescedDefinition>,
    proof_waiting: bool
}
pub(crate) struct BirthOwner
{
    id:         WindowId,
    owner:      mac_object_table::domain::DomainId,
    notice:     Arc<Notice>,
    new:        Arc<WindowJob>,
    calculate:  Arc<WindowJob>,
    dispose:    Arc<WindowJob>,
    registered: AtomicBool,
    abandoned:  AtomicBool,
    finished:   AtomicBool,
    state:      Spinlock<State>
}
pub(crate) enum CreationProgress
{
    Continue,
    Wait,
    Ready
}
pub(crate) enum CleanupProgress
{
    Wait,
    Finished
}
/// Payload may be lent only while the original Core is executing this service.
/// Drop restores SAME permit/state/proof; no user borrow or guard survives Yield.
#[must_use = "restore or publish the original unpublished Window payload"]
pub(crate) struct BirthLoan
{
    owner:               Arc<BirthOwner>,
    pub(super) prepared: Option<PreparedWindow>,
    pub(super) phase:    Phase,
    failure:             Option<Status>,
    plan:                Option<QuiescencePlan>,
    proof:               Option<QuiescedDefinition>,
    proof_waiting:       bool
}
impl Drop for BirthLoan
{
    fn drop(&mut self)
    {
        let done = matches!(self.phase, Phase::Published | Phase::Done);
        let wake = self.owner.with_state(|state| {
                                 state.prepared = self.prepared.take();
                                 state.phase = self.phase;
                                 state.failure = self.failure;
                                 state.plan = self.plan.take();
                                 state.proof = self.proof.take();
                                 state.proof_waiting = self.proof_waiting;
                                 state.busy = false;
                                 core::mem::take(&mut state.contender)
                             });
        if done
        {
            self.owner.finished.store(true, Ordering::Release);
        }
        if done || wake
        {
            self.owner.notice.request();
        }
    }
}
impl BirthLoan
{
    pub(crate) fn input(&mut self) -> &mut Option<PreparedWindow>
    {
        &mut self.prepared
    }

    /// Only the original canonical publisher consumes this Option. No ID lookup
    /// or numeric success can mark the prepared payload as transferred.
    pub(crate) fn published(&mut self) -> Result<(), Status>
    {
        if self.prepared.is_some()
        {
            return Err(BAD_ABI);
        }
        self.phase = Phase::Published;
        Ok(())
    }

    fn quiesce(&mut self) -> Result<(), Status>
    {
        let prepared = self.prepared.as_ref().ok_or(BAD_ABI)?;
        if self.plan.is_none() && self.proof.is_none()
        {
            self.plan = Some(prepared.birth_quiescence());
        }
        if self.proof.is_none()
        {
            let plan = self.plan.take().ok_or(BAD_ABI)?;
            match plan.acquire()
            {
                Ok(proof) => self.proof = Some(proof),
                Err((status, plan)) =>
                {
                    self.plan = Some(plan);
                    self.proof_waiting = true;
                    return Err(status);
                }
            }
        }
        if !self.proof.as_ref().is_some_and(|proof| prepared.birth_matches(proof))
        {
            return Err(ACCESS_DENIED);
        }
        self.owner.discard_unused();
        drop(self.prepared.take());
        drop(self.proof.take());
        self.phase = Phase::Done;
        self.proof_waiting = false;
        Ok(())
    }
}
impl BirthOwner
{
    fn with_state<R>(&self, operation: impl FnOnce(&mut State) -> R) -> R
    {
        InvocationGate::run(|| operation(&mut self.state.lock()))
    }

    pub(crate) fn prepare(mut prepared: PreparedWindow, waiter: Option<ThreadId>,
                          notice: Arc<Notice>)
                          -> Result<Arc<Self>, Status>
    {
        prepared.prepare_user_cleanup(notice.clone())?;
        let window = prepared.birth_window();
        let owner = prepared.owner().owner();
        let allocation = prepared.birth_allocation();
        crate::allocator::with_target(crate::allocator::Target::Owner {
            account: allocation, class: mac_kernel_model::policy::MemoryClass::Kernel
        }, || {
            let new = WindowJob::prepare_birth(window, window.id, owner, notice.clone(), waiter, definition::NEW)?;
            let calculate = WindowJob::prepare_birth(window, window.id, owner, notice.clone(), waiter, definition::CALCULATE)?;
            let dispose = prepared.birth_cleanup()?;
            Arc::try_new(Self { id: window.id, owner, notice, new, calculate, dispose,
                registered: AtomicBool::new(false), abandoned: AtomicBool::new(false), finished: AtomicBool::new(false),
                state: Spinlock::new(State { prepared: Some(prepared), phase: Phase::New, busy: false,
                    contender: false, failure: None, plan: None, proof: None, proof_waiting: false })
            }).map_err(|_| mac_abi::NO_MEMORY)
        })
    }

    pub(crate) fn window_id(&self) -> WindowId
    {
        self.id
    }

    pub(crate) fn storage_owner(&self) -> mac_object_table::domain::DomainId
    {
        self.owner
    }

    pub(crate) fn abandon(&self)
    {
        if !self.finished.load(Ordering::Acquire) && !self.abandoned.swap(true, Ordering::AcqRel)
        {
            self.notice.request();
        }
    }

    pub(crate) fn needs_cleanup(&self) -> bool
    {
        self.abandoned.load(Ordering::Acquire) && !self.finished.load(Ordering::Acquire)
    }

    pub(super) fn finished(&self) -> bool
    {
        self.finished.load(Ordering::Acquire)
    }

    pub(super) fn same_notice(&self, notice: &Arc<Notice>) -> bool
    {
        Arc::ptr_eq(&self.notice, notice)
    }

    pub(super) fn register(&self)
    {
        self.registered.store(true, Ordering::Release);
    }

    pub(super) fn registered(&self) -> bool
    {
        self.registered.load(Ordering::Acquire)
    }

    pub(crate) fn preparation(self: &Arc<Self>) -> Result<BirthLoan, Status>
    {
        if self.registered()
        {
            return Err(ACCESS_DENIED);
        }
        self.checkout()
    }

    pub(super) fn checkout(self: &Arc<Self>) -> Result<BirthLoan, Status>
    {
        self.with_state(|state| {
                if state.busy
                {
                    state.contender = true;
                    return Err(SERVICE_BUSY);
                }
                state.busy = true;
                Ok(BirthLoan { owner:         self.clone(),
                               prepared:      state.prepared.take(),
                               phase:         state.phase,
                               failure:       state.failure,
                               plan:          state.plan.take(),
                               proof:         state.proof.take(),
                               proof_waiting: state.proof_waiting })
            })
    }

    fn job(&self, phase: Phase) -> Option<&Arc<WindowJob>>
    {
        match phase
        {
            Phase::New => Some(&self.new),
            Phase::Calculate => Some(&self.calculate),
            Phase::Dispose => Some(&self.dispose),
            _ => None
        }
    }

    pub(crate) fn current_job(&self) -> Option<Arc<WindowJob>>
    {
        self.with_state(|state| self.job(state.phase).cloned())
    }

    fn discard_unused(&self)
    {
        self.new.discard_unused();
        self.calculate.discard_unused();
        self.dispose.discard_unused();
    }

    pub(crate) fn advance_creation(self: &Arc<Self>) -> Result<CreationProgress, CreationError>
    {
        if !self.registered() || self.abandoned.load(Ordering::Acquire)
        {
            return Err(CreationError::Stopping);
        }
        let mut loan = self.checkout().map_err(CreationError::Binding)?;
        if loan.phase == Phase::Ready
        {
            return Ok(CreationProgress::Ready);
        }
        let phase = loan.phase;
        let job = self.job(phase).ok_or(CreationError::InvalidRequest)?;
        let prepared = loan.prepared.as_ref().ok_or(CreationError::InvalidRequest)?;
        job.begin_prepared(prepared.birth_window(), self.id, self.owner)
           .map_err(CreationError::Binding)?;
        let result = job.advance().map_err(CreationError::Binding);
        match result
        {
            Ok(Progress::Pending) => Ok(CreationProgress::Wait),
            Ok(Progress::Complete(mut completion)) =>
            {
                let reply = completion.birth_reply(job).map_err(CreationError::Binding)?;
                let prepared = loan.prepared.as_mut().ok_or(CreationError::InvalidRequest)?;
                prepared.retain_author_state(reply).map_err(CreationError::Binding)?;
                let operation =
                    if phase == Phase::New { definition::NEW } else { definition::CALCULATE };
                let result = prepared.apply_user_reply(operation, reply);
                completion.accept_birth(job).map_err(CreationError::Binding)?;
                if result.is_err()
                {
                    loan.phase = Phase::Dispose;
                    self.abandon();
                }
                else
                {
                    loan.phase = if phase == Phase::New { Phase::Calculate } else { Phase::Ready };
                }
                result?;
                Ok(if loan.phase == Phase::Ready
                   {
                       CreationProgress::Ready
                   }
                   else
                   {
                       CreationProgress::Continue
                   })
            }
            Ok(Progress::Finished(_)) => Err(CreationError::InvalidRequest),
            Err(cause) =>
            {
                self.abandon();
                Err(cause)
            }
        }
    }

    pub(crate) fn publication(self: &Arc<Self>) -> Result<BirthLoan, Status>
    {
        if self.abandoned.load(Ordering::Acquire)
        {
            return Err(SERVICE_BUSY);
        }
        let loan = self.checkout()?;
        if loan.phase != Phase::Ready
        {
            return Err(SERVICE_BUSY);
        }
        Ok(loan)
    }

    /// Guard-free System maintenance; no author work or destruction occurs in
    /// the Window Manager's directory lock. One admitted phase is serviced.
    pub(crate) fn advance_cleanup(self: &Arc<Self>) -> Result<CleanupProgress, Status>
    {
        if !self.registered() || !self.needs_cleanup()
        {
            return Err(BAD_ABI);
        }
        let mut loan = self.checkout()?;
        if matches!(loan.phase, Phase::Published | Phase::Done)
        {
            return Ok(CleanupProgress::Finished);
        }
        if loan.phase == Phase::Refused
        {
            loan.quiesce()?;
            return Ok(CleanupProgress::Finished);
        }
        if matches!(loan.phase, Phase::New | Phase::Calculate)
        {
            let phase = loan.phase;
            let job = self.job(phase).ok_or(BAD_ABI)?;
            if !job.author_admitted()
            {
                if phase == Phase::New
                {
                    self.discard_unused();
                    drop(loan.prepared.take());
                    loan.phase = Phase::Done;
                    return Ok(CleanupProgress::Finished);
                }
                loan.phase = Phase::Dispose;
            }
            else
            {
                match job.advance()
                {
                    Ok(Progress::Pending) => return Ok(CleanupProgress::Wait),
                    Ok(Progress::Complete(mut completion)) =>
                    {
                        loan.prepared
                            .as_mut()
                            .ok_or(BAD_ABI)?
                            .retain_author_state(completion.birth_reply(job)?)?;
                        completion.accept_birth(job)?;
                        loan.phase = Phase::Dispose;
                    }
                    Ok(Progress::Finished(_)) => return Err(BAD_ABI),
                    Err(status) =>
                    {
                        loan.failure = Some(status);
                        loan.quiesce()?;
                        return Ok(CleanupProgress::Finished);
                    }
                }
            }
        }
        let prepared = loan.prepared.as_ref().ok_or(BAD_ABI)?;
        self.dispose.begin_prepared(prepared.birth_window(), self.id, self.owner)?;
        match self.dispose.advance()
        {
            Ok(Progress::Pending) => Ok(CleanupProgress::Wait),
            Ok(Progress::Complete(mut completion)) =>
            {
                let reply = completion.birth_reply(&self.dispose)?;
                loan.prepared.as_mut().ok_or(BAD_ABI)?.retain_author_state(reply)?;
                let result = reply.result;
                completion.accept_birth(&self.dispose)?;
                if result < 0
                {
                    let status = i32::try_from(result).unwrap_or(BAD_ABI);
                    loan.phase = Phase::Refused;
                    loan.failure = Some(status);
                    loan.quiesce()?;
                }
                else
                {
                    self.discard_unused();
                    drop(loan.prepared.take());
                    loan.phase = Phase::Done;
                }
                Ok(CleanupProgress::Finished)
            }
            Ok(Progress::Finished(_)) => Err(BAD_ABI),
            Err(status) =>
            {
                loan.failure = Some(status);
                loan.quiesce()?;
                Ok(CleanupProgress::Finished)
            }
        }
    }

    pub(super) fn source_changed(&self) -> bool
    {
        let changed = self.with_state(|state| core::mem::take(&mut state.proof_waiting));
        let mut progress = changed;
        for job in [&self.new, &self.calculate, &self.dispose]
        {
            progress |= job.resource_changed();
        }
        progress
    }
}
/// No semantic object remains here; take/drop outside Desktop so source/packet
/// header destruction and actual publication-reservation cleanup stay guard-free.
#[must_use = "drop the retired birth owner outside Desktop"]
pub(crate) struct RetiredBirth
{
    // Final source/packet/reservation custody is released only when this
    // guard-free retirement carrier drops, not by a metadata read.
    pub(super) _owner: Arc<BirthOwner>
}
