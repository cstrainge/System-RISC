//! Canonical per-CTRL disposal. Contexts observe work; they never own its prefix.
//! No callback runs while Desktop or a cell guard is held. The original Store
//! payload/ticket remains retained until actual author return or source proof.
#![forbid(unsafe_code)]
use alloc::sync::{Arc, Weak};
use core::sync::atomic::{AtomicBool, Ordering};

use mac_object_table::store::CompletionKey;
use spinning_top::Spinlock;

use super::*;
use crate::toolbox::resources::definitions::{QuiescedDefinition, QuiescencePlan,
                                             callbacks::{CompletionTarget, Notice, Pending,
                                                         PreparedControlDisposal, packet::Request}};

#[derive(Clone, Copy)]
enum DisposalFailure
{
    Author
    {
        control: mac_object_table::domain::ResourceKey, reply: i64
    },
    Transport
    {
        control: mac_object_table::domain::ResourceKey, status: Status
    }
}
impl DisposalFailure
{
    fn status(self) -> Status
    {
        match self
        {
            Self::Author { reply,
                           .. } => Status::try_from(reply).unwrap_or(BAD_ABI),
            Self::Transport { status,
                              .. } => status
        }
    }

    fn cause(self) -> crate::toolbox::handles::cleanup::Cause
    {
        match self
        {
            Self::Author { control,
                           reply } =>
            {
                crate::toolbox::handles::cleanup::Cause::ControlDefinitionDisposal { control,
                                                                                     reply }
            }
            Self::Transport { control,
                              status } =>
            {
                crate::toolbox::handles::cleanup::Cause::ControlDisposalPending { control,
                                                                                  status }
            }
        }
    }
}

/// Guard-free cleanup admission owned by a not-yet-published native control.
/// It carries no live ID/Activity and pins no execution gate while parked.
pub(crate) struct ControlCleanup
{
    cell: Arc<ControlDisposal>,
    slot: Arc<DisposalSlot>
}
impl ControlCleanup
{
    pub(crate) fn prepare(reference: crate::toolbox::resources::definitions::DefinitionRef,
                          call: &ControlCall, storage: &OwnerRef, notice: Arc<Notice>,
                          allocation: &crate::machine::scheduler::accounting::AllocationAuthority)
                          -> Result<Self, Status>
    {
        if allocation.owner() != storage.owner()
        {
            return Err(ACCESS_DENIED);
        }
        let target =
            crate::allocator::Target::Owner { account: allocation.clone(),
                                              class:
                                                  mac_kernel_model::policy::MemoryClass::Kernel };
        crate::allocator::with_target(target, || {
            let (cell, slot) = ControlDisposal::new(notice, storage)?;
            let call = super::super::validation_state(call);
            let id = super::super::definition_id(&call)?;
            let request = super::super::user_disposal_request(&call, id);
            let stock = PreparedControlDisposal::prepare(reference,
                                                         &request,
                                                         CompletionTarget::System(cell.notice
                                                                                      .clone()),
                                                         allocation)?;
            // Brand-new private Cell; no publication or observer can see it.
            {
                let mut guard = cell.state.lock();
                let Some(state) = guard.as_mut()
                else
                {
                    return Err(BAD_ABI);
                };
                state.prepared = Some(stock);
            }
            Ok(Self { cell,
                      slot })
        })
    }

    pub(super) fn matches(&self, storage: &OwnerRef,
                          reference: crate::toolbox::resources::definitions::DefinitionRef)
                          -> bool
    {
        self.cell.owner.owner() == storage.owner()
        && self.cell.key().is_none()
        && self.cell
               .state
               .lock()
               .as_ref()
               .and_then(|state| state.prepared.as_ref())
               .is_some_and(|stock| stock.matches(reference))
    }

    pub(super) fn into_parts(self) -> (Arc<ControlDisposal>, Arc<DisposalSlot>)
    {
        (self.cell, self.slot)
    }
}

struct State
{
    source:       Option<QuiescencePlan>,
    request:      Option<Request>,
    prepared:     Option<PreparedControlDisposal>,
    pending:      Option<Arc<Pending>>,
    proof:        Option<QuiescedDefinition>,
    dependencies: [Option<Arc<ControlDisposal>>; 2],
    failure:      Option<Status>,
    returned:     bool
}
/// This cell is admitted with the original ordinary CTRL payload. Prepaid
/// Process cleanup retains its original definition and weak canonical worker,
/// but idle cells own no execution pin, Activity, caller or disposal ticket.
pub(crate) struct ControlDisposal
{
    owner:             OwnerRef,
    payload_destroyed: AtomicBool,
    identity:          Spinlock<Option<(CompletionKey, u64)>>,
    slot:              Spinlock<Weak<DisposalSlot>>,
    waiter:            spin::Once<crate::context::ThreadId>,
    wake_pending:      AtomicBool,
    notice:            Arc<Notice>,
    active:            AtomicBool,
    author_done:       AtomicBool,
    retired:           AtomicBool,
    blocked:           AtomicBool,
    terminal:          Spinlock<Option<DisposalFailure>>,
    failure_reported:  AtomicBool,
    state:             Spinlock<Option<State>>
}
impl ControlDisposal
{
    pub(super) fn new(parent: Arc<Notice>, owner: &OwnerRef)
                      -> Result<(Arc<Self>, Arc<DisposalSlot>), Status>
    {
        let notice = Notice::linked(parent)?;
        let cell = Arc::try_new(Self {terminal:Spinlock::new(None),failure_reported:AtomicBool::new(false),owner:owner.clone(),payload_destroyed:AtomicBool::new(false), waiter:spin::Once::new(), wake_pending:AtomicBool::new(false), identity: Spinlock::new(None), slot:Spinlock::new(Weak::new()), notice,
            active: AtomicBool::new(false), author_done: AtomicBool::new(false),
            retired: AtomicBool::new(false), blocked: AtomicBool::new(false),
            state: Spinlock::new(Some(State { source: None, request: None, prepared: None,
                pending: None, proof: None, dependencies: [None,None], failure: None,
                returned: false })) }).map_err(|_| NO_MEMORY)?;
        let slot =
            Arc::try_new(DisposalSlot { cell:          cell.clone(),
                                        next:          Spinlock::new(None),
                                        ui_pending:    AtomicBool::new(false),
                                        clear_outline: AtomicBool::new(false),
                                        removed:       Spinlock::new(None) }).map_err(|_| {
                                                                                 NO_MEMORY
                                                                             })?;
        *cell.slot.lock() = Arc::downgrade(&slot);
        Ok((cell, slot))
    }

    // Called only by the successful canonical publication, before returning ID.
    pub(super) fn bind(&self, key: CompletionKey, window: u64)
    {
        *self.identity.lock() = Some((key, window));
    }

    pub(super) fn key(&self) -> Option<CompletionKey>
    {
        self.identity.lock().map(|(key, _)| key)
    }

    fn window(&self) -> Option<u64>
    {
        self.identity.lock().map(|(_, window)| window)
    }

    pub(super) fn active(&self) -> bool
    {
        self.active.load(Ordering::Acquire)
    }

    pub(crate) fn done(&self) -> bool
    {
        self.retired.load(Ordering::Acquire)
    }

    pub(super) fn author_done(&self) -> bool
    {
        self.author_done.load(Ordering::Acquire)
    }

    pub(super) fn arm(&self, source: Option<QuiescencePlan>, request: Option<Request>,
                      dependencies: [Option<Arc<Self>>; 2],
                      waiter: Option<crate::context::ThreadId>)
    {
        // Admission validated all cells before arming any of them. This is the
        // only Idle -> requested transition; no retry reconstructs a request.
        let mut guard = self.state.lock();
        if let Some(state) = guard.as_mut()
        {
            state.returned = source.is_none();
            state.source = source;
            state.request = request;
            state.dependencies = dependencies;
            if let Some(waiter) = waiter
            {
                self.waiter.call_once(|| waiter);
            }
            self.active.store(true, Ordering::Release);
            self.notice.request();
        }
    }

    pub(super) fn source_returned(&self)
    {
        if self.blocked.swap(false, Ordering::AcqRel)
        {
            self.notice.request();
            self.wake_pending.store(true, Ordering::Release);
        }
    }

    pub(crate) fn waiter(&self) -> Option<crate::context::ThreadId>
    {
        self.waiter.get().copied()
    }

    pub(crate) fn wait_ready(&self) -> bool
    {
        self.done() || self.notice.ready()
    }

    pub(crate) fn deliver_wake(&self)
    {
        if self.wake_pending.swap(false, Ordering::AcqRel)
        {
            if let Some(waiter) = self.waiter()
            {
                crate::machine::scheduler::callbacks::wake_waiter(waiter);
            }
        }
    }

    fn needs_author_or_source(&self) -> bool
    {
        if self.author_done()
        {
            return false;
        }
        let state = self.state.lock();
        let Some(state) = state.as_ref()
        else
        {
            return true;
        };
        if state.returned
        {
            return false;
        }
        // A negative return is not successful teardown: original-source
        // quiescence is still required and this live role would prevent it.
        !state.pending
              .as_ref()
              .and_then(|pending| pending.returned_result())
              .is_some_and(|result| result >= 0)
    }

    pub(super) fn validate_stock_replacement(&self) -> Result<(), Status>
    {
        if self.active() || self.state.lock().is_none()
        {
            return Err(SERVICE_BUSY);
        }
        Ok(())
    }

    pub(super) fn replace_stock(&self, stock: Option<PreparedControlDisposal>)
                                -> Option<PreparedControlDisposal>
    {
        let mut state = self.state.lock();
        // The caller retains the same exclusive original CTRL claim and did
        // this cell preflight before the common infallible state publication.
        state.as_mut().and_then(|state| core::mem::replace(&mut state.prepared, stock))
    }

    fn terminal(&self) -> Option<DisposalFailure>
    {
        *self.terminal.lock()
    }

    fn record_terminal(&self, failure: DisposalFailure)
    {
        let mut terminal = self.terminal.lock();
        if terminal.is_none()
        {
            *terminal = Some(failure);
        }
    }

    /// One guard-free turn per actual completion/availability edge. A cold
    /// pending callback is never recreated or polled speculatively.
    pub(crate) fn advance(&self) -> Result<(), Status>
    {
        if !self.active() || self.author_done() || !self.notice.ready()
        {
            return Ok(());
        }
        let state = self.state.lock().take().ok_or(SERVICE_BUSY)?;
        self.notice.take();
        // Arm before testing dependencies, source gates or allocation. A real
        // return may race the refusal; never arm only after that edge passed.
        self.blocked.store(true, Ordering::Release);
        let mut turn = Turn { cell:  self,
                              state: Some(state) };
        let state = turn.state.as_mut().ok_or(BAD_ABI)?;
        if state.dependencies.iter().flatten().any(|cell| !cell.done())
        {
            self.blocked.store(true, Ordering::Release);
            return Ok(());
        }
        for child in state.dependencies.iter().flatten()
        {
            if let Some(failure) = child.terminal()
            {
                self.record_terminal(failure);
            }
        }
        if state.returned
        {
            self.blocked.store(false, Ordering::Release);
            self.author_done.store(true, Ordering::Release);
            return Ok(());
        }
        if let Some(pending) = state.pending.as_ref()
        {
            if !pending.ready()
            {
                self.blocked.store(false, Ordering::Release);
                return Ok(());
            }
            match pending.poll()
            {
                Ok(Some(reply)) =>
                {
                    let result = reply.result;
                    drop(reply);
                    drop(state.pending.take());
                    if result < 0
                    {
                        let control = self.key().ok_or(BAD_ABI)?.resource_key();
                        self.record_terminal(DisposalFailure::Author { control,
                                                                       reply: result });
                        state.failure.get_or_insert(Status::try_from(result).unwrap_or(BAD_ABI));
                        // A failed destructor may already have effects. Do not
                        // retry it or infer a successful teardown from RETURN.
                    }
                    else
                    {
                        state.returned = true;
                        self.blocked.store(false, Ordering::Release);
                        self.author_done.store(true, Ordering::Release);
                        crate::kernel::window_sources_changed();
                        return Ok(());
                    }
                }
                Ok(None) => return Ok(()),
                Err(status) =>
                {
                    let control = self.key().ok_or(BAD_ABI)?.resource_key();
                    self.record_terminal(DisposalFailure::Transport { control,
                                                                      status });
                    state.failure.get_or_insert(status);
                    drop(state.pending.take());
                    // Never resend a callback which may have executed. Only
                    // authentic source quiescence can abandon its author state.
                    drop(state.request.take());
                    drop(state.prepared.take());
                }
            }
        }
        // Source proof is attempted before allocation/activation. A closed
        // source cannot be resumed just to run DISPOSE.
        if let Some(plan) = state.source.take()
        {
            match plan.acquire()
            {
                Ok(proof) =>
                {
                    drop(state.request.take());
                    drop(state.prepared.take());
                    state.proof = Some(proof);
                    state.returned = true;
                    self.blocked.store(false, Ordering::Release);
                    self.author_done.store(true, Ordering::Release);
                    crate::kernel::window_sources_changed();
                    return Ok(());
                }
                Err((_, plan)) => state.source = Some(plan)
            }
        }
        if state.failure.is_some() && state.request.is_none() && state.prepared.is_none()
        {
            self.blocked.store(true, Ordering::Release);
            return Ok(());
        }
        // Every Process CDEF publication/source change admitted this exact
        // fixed carrier before author effects. Cleanup never allocates one.
        let completion = match self.waiter()
        {
            Some(thread) =>
            {
                CompletionTarget::ThreadAndSystem { thread,
                                                    notice: self.notice.clone() }
            }
            None => CompletionTarget::System(self.notice.clone())
        };
        let latest = state.request.as_ref().ok_or(BAD_ABI)?;
        let prepared = state.prepared.take().ok_or(BAD_ABI)?;
        match prepared.activate(latest, completion)
        {
            Ok(pending) =>
            {
                self.blocked.store(false, Ordering::Release);
                state.pending = Some(pending);
                drop(state.request.take());
            }
            Err((status, prepared)) =>
            {
                state.prepared = Some(prepared);
                state.failure.get_or_insert(status);
                self.blocked.store(true, Ordering::Release);
            }
        }
        Ok(())
    }

    pub(super) fn retired(&self)
    {
        self.retired.store(true, Ordering::Release);
        self.notice.request();
        self.wake_pending.store(true, Ordering::Release);
    }

    fn refresh_ack(&self) -> Result<(), Status>
    {
        if !self.payload_destroyed.load(Ordering::Acquire) || self.done()
        {
            return Ok(());
        }
        let key = self.key().ok_or(BAD_ABI)?.resource_key();
        if self.owner.owner() != key.owner()
        {
            return Err(BAD_ABI);
        }
        if self.owner.pending(key) || self.owner.contains(key)
        {
            // Close ACK-before-registration: the hint is armed before the
            // second authoritative same-key observation, never after refusal.
            crate::kernel::arm_control_acknowledgement();
            if self.owner.pending(key) || self.owner.contains(key)
            {
                return Ok(());
            }
        }
        self.retired();
        self.finish();
        Ok(())
    }

    pub(super) fn acknowledged(&self)
    {
        if self.payload_destroyed.load(Ordering::Acquire) && !self.done()
        {
            self.notice.request();
        }
    }

    pub(super) fn finish(&self)
    {
        // Called only after payload destruction and the original owner/key
        // checks observed the actual ACK, including any quarantined completion.
        // Proof and original source pins end outside Desktop, after payload.
        let retired = self.state.lock().take();
        drop(retired);
    }
}
struct Turn<'a>
{
    cell:  &'a ControlDisposal,
    state: Option<State>
}
impl Drop for Turn<'_>
{
    fn drop(&mut self)
    {
        *self.cell.state.lock() = self.state.take();
        // Forward an actual edge received during this affine checkout.
        if self.cell.notice.ready()
        {
            self.cell.notice.request();
        }
    }
}


/// Actual preadmitted retirement storage. Object owns Cell, Cell holds only a
/// weak Slot, and Slot may own Removed<Object>: there is no authority cycle.
pub(crate) struct DisposalSlot
{
    cell:          Arc<ControlDisposal>,
    next:          Spinlock<Option<Arc<DisposalSlot>>>,
    ui_pending:    AtomicBool,
    clear_outline: AtomicBool,
    removed:       Spinlock<Option<Retired>>
}
impl DisposalSlot
{
    pub(crate) fn next(&self) -> Option<Arc<Self>>
    {
        self.next.lock().clone()
    }

    pub(crate) fn advance(&self) -> Result<(), Status>
    {
        let result = self.cell.advance();
        if let Some(failure) = self.cell.terminal()
        {
            if !self.cell.failure_reported.swap(true, Ordering::AcqRel)
            {
                serial!("CONTROL_DISPOSAL_RETAINED cause={}", failure.cause());
            }
        }
        result
    }

    pub(crate) fn deliver_wake(&self)
    {
        self.cell.deliver_wake();
    }

    pub(crate) fn window(&self) -> Option<u64>
    {
        self.cell.window()
    }

    pub(crate) fn ui_pending(&self) -> bool
    {
        self.ui_pending.load(Ordering::Acquire)
    }

    pub(crate) fn clear_outline(&self) -> bool
    {
        self.clear_outline.load(Ordering::Acquire)
    }

    pub(crate) fn ui_refused(&self)
    {
        self.cell.blocked.store(true, Ordering::Release);
    }

    pub(crate) fn drain(&self)
    {
        if self.ui_pending() || !self.cell.author_done()
        {
            return;
        }
        // Consume the real completion hint before checking its actual key.
        // An unrelated ACK must not leave a permanently-ready native waiter.
        self.cell.notice.take();
        let removed = self.removed.lock().take();
        if let Some(removed) = removed
        {
            drop(removed); // SAME original CTRL payload and real ledger ACK
            self.cell.payload_destroyed.store(true, Ordering::Release);
        }
        // Pool completion may have quarantined an ACK after payload Drop. Only
        // the original owner/key observations can make this cell terminal.
        if let Err(status) = self.cell.refresh_ack()
        {
            serial!("CONTROL_DISPOSAL_ACK_RETAINED status={status}");
        }
        if self.cell.done()
        {
            crate::kernel::window_sources_changed();
        }
    }
}
struct DisposalRequest
{
    clear_outline: bool,
    cell:          Arc<ControlDisposal>,
    source:        Option<QuiescencePlan>,
    request:       Option<Request>,
    dependencies:  [Option<Arc<ControlDisposal>>; 2],
    waiter:        Option<crate::context::ThreadId>
}
pub(crate) struct DisposalObserver
{
    root:   Arc<ControlDisposal>,
    output: super::super::effects::DetachedCall
}
impl DisposalObserver
{
    pub(crate) fn advance(&self) -> Result<bool, Status>
    {
        if self.root.done()
        {
            return match self.root.terminal()
            {
                Some(failure) => Err(failure.status()),
                None => Ok(true)
            };
        }
        let mut current = self.root.clone();
        loop
        {
            let child = {
                let state = current.state.lock();
                state.as_ref().and_then(|state| {
                                  state.dependencies
                                       .iter()
                                       .flatten()
                                       .find(|cell| !cell.done())
                                       .cloned()
                              })
            };
            if let Some(child) = child
            {
                current = child;
                continue;
            }
            current.advance()?;
            if let Some(failure) = current.terminal()
            {
                return Err(failure.status());
            }
            let state = current.state.lock();
            if let Some(status) = state.as_ref()
                                       .filter(|state| state.pending.is_none() && !state.returned)
                                       .and_then(|state| state.failure)
            {
                return Err(status);
            }
            return Ok(false);
        }
    }

    pub(crate) fn wait(&self) -> Arc<ControlDisposal>
    {
        let mut current = self.root.clone();
        loop
        {
            let child = {
                let state = current.state.lock();
                state.as_ref().and_then(|state| {
                                  state.dependencies
                                       .iter()
                                       .flatten()
                                       .find(|cell| !cell.done())
                                       .cloned()
                              })
            };
            match child
            {
                Some(child) => current = child,
                None => return current
            }
        }
    }

    pub(crate) fn output(&self) -> ControlCall
    {
        self.output.wire()
    }
}
// Each Slot belongs to exactly one list, idle or active. It is unlinked (and
// its next cleared) before transfer/destruction; cancellation cannot drop a
// recursive chain or leave caller-charged global Vec capacity resident.
fn unlink(head: &mut Option<Arc<DisposalSlot>>, target: &Arc<DisposalSlot>)
          -> Option<Arc<DisposalSlot>>
{
    if head.as_ref().is_some_and(|slot| Arc::ptr_eq(slot, target))
    {
        let slot = head.take()?;
        *head = slot.next.lock().take();
        return Some(slot);
    }
    let mut previous = head.clone();
    while let Some(slot) = previous
    {
        let mut next = slot.next.lock();
        if next.as_ref().is_some_and(|slot| Arc::ptr_eq(slot, target))
        {
            let removed = next.take()?;
            *next = removed.next.lock().take();
            return Some(removed);
        }
        let following = next.clone();
        drop(next);
        previous = following;
    }
    None
}
impl Manager
{
    pub(super) fn publish_disposal_slot(&mut self, slot: Option<Arc<DisposalSlot>>,
                                        key: CompletionKey, window: u64)
    {
        if let Some(slot) = slot
        {
            slot.cell.bind(key, window);
            *slot.next.lock() = self.disposal_idle.take();
            self.disposal_idle = Some(slot);
        }
    }

    pub(crate) fn disposal_notice(&self) -> Arc<Notice>
    {
        self.disposal_notice.clone()
    }

    pub(crate) fn take_disposal_notice(&self) -> bool
    {
        self.disposal_notice.take()
    }

    pub(crate) fn disposal_work(&self) -> Option<Arc<DisposalSlot>>
    {
        self.disposal_active.clone()
    }

    pub(crate) fn disposal_sources_returned(&self)
    {
        let mut current = self.disposal_active.clone();
        while let Some(slot) = current
        {
            current = slot.next();
            slot.cell.source_returned();
        }
    }

    pub(crate) fn disposal_acknowledgements(&self)
    {
        let mut current = self.disposal_active.clone();
        while let Some(slot) = current
        {
            current = slot.next();
            slot.cell.acknowledged();
        }
    }

    pub(crate) fn take_finished_disposal(&mut self) -> Option<Arc<DisposalSlot>>
    {
        let mut current = self.disposal_active.clone();
        while let Some(slot) = current
        {
            current = slot.next();
            if slot.cell.done()
            {
                return unlink(&mut self.disposal_active, &slot);
            }
        }
        None
    }

    fn disposal_cell(&self, id: u64) -> Option<Arc<ControlDisposal>>
    {
        for head in [&self.disposal_idle, &self.disposal_active]
        {
            let mut current = head.clone();
            while let Some(slot) = current
            {
                current = slot.next();
                if slot.cell.key().is_some_and(|key| key.id() == id)
                {
                    return Some(slot.cell.clone());
                }
            }
        }
        None
    }

    pub(super) fn disposing_owner(&self, id: u64) -> Option<mac_object_table::domain::DomainId>
    {
        let cell = self.disposal_cell(id)?;
        (cell.active() && !cell.done()).then(|| cell.key().map(|key| key.owner())).flatten()
    }

    fn capture_disposal(&self, object: &Object) -> Result<DisposalRequest, Status>
    {
        let cell = object.disposal.as_ref().ok_or(BAD_ABI)?.clone();
        let (source, request) = if object.definition.is_process_source()
        {
            let mut call = object.state.spec.draw();
            call.reflow = u32::from(object.state.layout.is_some());
            (Some(object.definition.quiescence_plan()),
             Some(super::super::user_disposal_request(&call, object.definition.resource_id())))
        }
        else
        {
            (None, None)
        };
        let dependencies = [object.tree.first, object.tree.next].map(|id| self.disposal_cell(id));
        Ok(DisposalRequest { clear_outline: object.state.splitter.is_some(),
                             cell,
                             source,
                             request,
                             dependencies,
                             waiter: None })
    }

    fn request_disposal(&mut self, request: DisposalRequest) -> Result<(), Status>
    {
        let DisposalRequest { clear_outline,
                              cell,
                              source,
                              request,
                              dependencies,
                              waiter } = request;
        if cell.active()
        {
            return Ok(());
        }
        let key = cell.key().ok_or(BAD_ABI)?;
        let slot = cell.slot.lock().upgrade().ok_or(BAD_ABI)?;
        let slot = unlink(&mut self.disposal_idle, &slot).ok_or(SERVICE_BUSY)?;
        if self.objects.completion_key(key.id()) == Some(key)
        {
            // Whole selected trees are frozen before author execution. Keep
            // their original edges so later owner-walk members resolve the
            // same full-generation dependency cells after payload extraction.
            *slot.removed.lock() = self.objects.remove(key.id());
            slot.ui_pending.store(true, Ordering::Release);
        }
        slot.clear_outline.store(clear_outline, Ordering::Release);
        *slot.next.lock() = self.disposal_active.take();
        self.disposal_active = Some(slot);
        cell.arm(source, request, dependencies, waiter);
        Ok(())
    }

    pub(crate) fn disposal_observed(&self, id: u64, owner: mac_object_table::domain::DomainId)
                                    -> bool
    {
        let mut current = self.disposal_active.clone();
        while let Some(slot) = current
        {
            current = slot.next();
            if slot.cell.key().is_some_and(|key| key.id() == id && key.owner() == owner)
            {
                return true;
            }
        }
        false
    }

    pub(crate) fn disposal_pending_window(&self, window: u64) -> bool
    {
        let mut current = self.disposal_active.clone();
        while let Some(slot) = current
        {
            current = slot.next();
            if slot.cell.window() == Some(window) && !slot.cell.done()
            {
                return true;
            }
        }
        false
    }

    pub(crate) fn begin_control_disposal(&mut self, call: &ControlCall, owner: &OwnerRef)
                                         -> Result<Option<DisposalObserver>, Status>
    {
        let output = super::super::effects::DetachedCall::capture(call)?;
        if call.reserved != 0
        {
            return Err(BAD_ARGUMENT);
        }
        let waiter = crate::context::current_thread_context().identity().thread;
        let mut current = self.disposal_active.clone();
        while let Some(slot) = current
        {
            current = slot.next();
            let root = &slot.cell;
            if root.key().is_some_and(|key| key.id() == call.id && key.owner() == owner.owner())
            {
                if !root.done() && root.waiter() != Some(waiter)
                {
                    return Err(SERVICE_BUSY);
                }
                return Ok(Some(DisposalObserver { root: root.clone(),
                                                  output }));
            }
        }
        if !self.disposal_needs_author(call, owner.owner().incarnation())?
        {
            return Ok(None);
        }
        self.no_claims_on_tree(call.id)?;
        let ids = self.subtree(call.id)?;
        let mut requests = Vec::new();
        requests.try_reserve_exact(ids.len()).map_err(|_| NO_MEMORY)?;
        for id in ids.iter().rev()
        {
            let object = self.get(*id, owner.owner().incarnation())?;
            let mut request = self.capture_disposal(object)?;
            request.waiter = Some(waiter);
            requests.push(request);
        }
        if let Ok(execution) = crate::machine::scheduler::callbacks::authenticate_current()
        {
            if ids.iter().any(|id| {
                             self.objects
                                 .get(*id)
                                 .is_some_and(|object| {
                                     object.definition.callback_waits_on(&execution)
                                 })
                         })
            {
                return Err(SERVICE_BUSY);
            }
        }
        let root = requests.last().ok_or(BAD_ABI)?.cell.clone();
        for request in requests
        {
            self.request_disposal(request)?;
        }
        Ok(Some(DisposalObserver { root,
                                   output }))
    }

    pub(super) fn release_control(&mut self, ticket: mac_object_table::domain::ReleasedResource)
                                  -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        use crate::toolbox::handles::cleanup::Failure;
        let Some(object) = self.objects.releasing(&ticket)
        else
        {
            return self.objects.release(ticket).map(drop).map_err(Failure::from);
        };
        let requested = object.disposal.as_ref().is_some_and(|cell| cell.active());
        if object.claimed() && !requested
        {
            return Err(Failure::control_operation_pending(ticket));
        }
        if object.definition.is_process_source() || requested
        {
            let cell = if requested
            {
                match object.disposal.as_ref()
                {
                    Some(cell) => cell.clone(),
                    None => return Err(Failure::control_disposal_pending(ticket, BAD_ABI))
                }
            }
            else
            {
                let request = match self.capture_disposal(object)
                {
                    Ok(request) => request,
                    Err(status) => return Err(Failure::control_disposal_pending(ticket, status))
                };
                let cell = request.cell.clone();
                if let Err(status) = self.request_disposal(request)
                {
                    return Err(Failure::control_disposal_pending(ticket, status));
                }
                cell
            };
            let Some(slot) = cell.slot.lock().upgrade()
            else
            {
                return Err(Failure::control_disposal_pending(ticket, BAD_ABI));
            };
            let mut pending = slot.removed.lock();
            if pending.is_some()
            {
                return Err(Failure::control_operation_pending(ticket));
            }
            slot.ui_pending.store(true, Ordering::Release);
            *pending = Some(self.objects.release(ticket).map_err(Failure::from)?);
            self.disposal_notice.request();
            return Ok(());
        }
        let cell = object.disposal.clone();
        if let Some(cell) = cell
        {
            self.retire_idle_metadata(&cell);
        }
        drop(self.objects.release(ticket).map_err(Failure::from)?);
        crate::kernel::window_sources_changed();
        Ok(())
    }

    // Existing trusted synchronous teardown retains its prior payload/ACK
    // behavior. Only newly introduced idle cleanup metadata is deferred here.
    pub(super) fn retire_idle_metadata(&mut self, cell: &Arc<ControlDisposal>)
    {
        let slot = cell.slot.lock().upgrade();
        if let Some(slot) = slot.and_then(|slot| unlink(&mut self.disposal_idle, &slot))
        {
            cell.payload_destroyed.store(true, Ordering::Release);
            cell.author_done.store(true, Ordering::Release);
            *slot.next.lock() = self.disposal_active.take();
            self.disposal_active = Some(slot);
            self.disposal_notice.request();
        }
    }

    pub(super) fn retire_idle_window_metadata(&mut self, window: u64)
    {
        let mut current = self.disposal_idle.clone();
        while let Some(slot) = current
        {
            current = slot.next();
            if slot.cell.window() == Some(window)
            {
                self.retire_idle_metadata(&slot.cell);
            }
        }
    }

    pub(super) fn retire_idle_owner_metadata(&mut self, owner: u64)
    {
        let mut current = self.disposal_idle.clone();
        while let Some(slot) = current
        {
            current = slot.next();
            if slot.cell.key().is_some_and(|key| key.owner().incarnation() == owner)
            {
                self.retire_idle_metadata(&slot.cell);
            }
        }
    }
}

impl Manager
{
    /// Called only after an actual close/removal intent is retained by its
    /// canonical Window owner. Busy retains every original child and ticket.
    pub(crate) fn request_window_disposal(&mut self, window: u64) -> Result<(), Status>
    {
        if self.disposal_pending_window(window)
        {
            return Err(SERVICE_BUSY);
        }
        if !self.objects
                .retained_values()
                .any(|object| object.window == window && object.definition.is_process_source())
        {
            return self.prepare_remove_window(window);
        }
        self.request_matching_disposals(|object| object.window == window)?;
        Err(SERVICE_BUSY)
    }

    fn request_matching_disposals(&mut self, select: impl Fn(&Object) -> bool)
                                  -> Result<(), Status>
    {
        // No effects precede validation of all existing claims and all required
        // preadmitted cells. No allocation or author call occurs during arm.
        for object in self.objects.retained_values().filter(|object| select(object))
        {
            if object.claimed()
            {
                return Err(SERVICE_BUSY);
            }
            if object.disposal.is_none()
            {
                return Err(BAD_ABI);
            }
        }
        let mut current = self.disposal_idle.clone();
        while let Some(slot) = current
        {
            current = slot.next();
            let Some(key) = slot.cell.key()
            else
            {
                return Err(BAD_ABI);
            };
            let Some(object) = self.objects.completing(&key)
            else
            {
                continue;
            };
            if !select(object)
            {
                continue;
            }
            let request = self.capture_disposal(object)?;
            self.request_disposal(request)?;
        }
        Ok(())
    }

    pub(super) fn request_owner_disposal(&mut self, owner: u64) -> Result<(), Status>
    {
        if self.objects
               .retained_values()
               .any(|object| object.recipient == owner && object.definition.is_process_source())
        {
            self.request_matching_disposals(|object| object.recipient == owner)?;
            return Err(SERVICE_BUSY);
        }
        Ok(())
    }

    pub(super) fn request_tree_disposal(&mut self, id: u64, owner: u64) -> Result<bool, Status>
    {
        let object = self.get(id, owner)?;
        if object.tree.parent != 0
        {
            return Err(ACCESS_DENIED);
        }
        let ids = self.subtree(id)?;
        if !ids.iter().any(|id| {
                          self.objects
                              .get(*id)
                              .is_some_and(|object| object.definition.is_process_source())
                      })
        {
            return Ok(false);
        }
        self.no_claims_on_tree(id)?;
        let mut requests = Vec::new();
        requests.try_reserve_exact(ids.len()).map_err(|_| NO_MEMORY)?;
        for id in ids.iter().rev()
        {
            requests.push(self.capture_disposal(self.get(*id, owner)?)?);
        }
        for request in requests
        {
            self.request_disposal(request)?;
        }
        Ok(true)
    }
}

impl Manager
{
    pub(crate) fn finish_disposal_ui(&self, window: u64)
    {
        let mut current = self.disposal_active.clone();
        while let Some(slot) = current
        {
            current = slot.next();
            if slot.window() == Some(window)
            {
                slot.ui_pending.store(false, Ordering::Release);
            }
        }
    }
}

impl Manager
{
    pub(crate) fn window_disposal_waits_on(&self, window: u64,
                                           execution:&crate::machine::scheduler::callbacks::CallbackExecution)
                                           -> bool
    {
        if self.objects.retained_values().any(|object|object.window==window
            && object.definition.callback_waits_on(execution)) {return true;}
        let mut current = self.disposal_active.clone();
        while let Some(slot) = current
        {
            current = slot.next();
            if slot.window() == Some(window)
               && slot.cell.needs_author_or_source()
               && slot.removed
                      .lock()
                      .as_ref()
                      .is_some_and(|object| object.definition.callback_waits_on(execution))
            {
                return true;
            }
        }
        false
    }
}
