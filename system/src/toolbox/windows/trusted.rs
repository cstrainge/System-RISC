//! Canonical trusted-WDEF close custody, including prepaid recovery endpoints.
//! Dormant stock has no Window identity or Activity; only a real WIND birth
//! binds it. CTRL disposal, WDEF disposal, UI completion and WIND ACK differ.
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::{ACCESS_DENIED, BAD_ABI, NO_MEMORY, NOT_FOUND, SERVICE_BUSY, Status};
use mac_object_table::{domain::{DomainId, ReleasedResource},
                       registry::CleanupFailure};
use spinning_top::Spinlock;

use super::{operations::{CleanupReason, CloseCleanup},
            *};
use crate::{context::ThreadId, machine::scheduler::accounting::AllocationAuthority,
            toolbox::resources::definitions::callbacks::Notice};

#[derive(Clone, Copy)]
pub(crate) struct TrustedClosePlan
{
    window:   Window,
    storage:  DomainId,
    revision: u64
}
impl TrustedClosePlan
{
    pub(super) fn from_birth(window: Window, storage: DomainId, revision: u64) -> Self
    {
        Self { window,
               storage,
               revision }
    }

    /// Guard-free ordinary Process preparation before any close effects.
    pub(crate) fn prepare(self, owner: &OwnerRef, allocation: &AllocationAuthority,
                          notice: Arc<Notice>, waiter: Option<ThreadId>)
                          -> Result<TrustedCloseHandle, Status>
    {
        if allocation.owner() != self.storage || owner.owner() != self.storage
        {
            return Err(ACCESS_DENIED);
        }
        crate::allocator::with_target(crate::allocator::Target::Owner {
            account: allocation.clone(), class: mac_kernel_model::policy::MemoryClass::Kernel
        }, || {
            let header = TrustedCloseHeader::prepare(Origin::Process { allocation: allocation.clone(), owner: owner.clone() }, notice)?;
            header.bind(self, waiter)
        })
    }
}
enum Origin
{
    System(crate::kernel::SystemResources),
    Process
    {
        allocation: AllocationAuthority,
        owner:      OwnerRef
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ui
{
    BeforeReply,
    Pending,
    Claimed,
    Blocked,
    Done
}
struct State
{
    plan:            Option<TrustedClosePlan>,
    admitted:        bool,
    result:          Option<Result<WindowId, CloseError>>,
    reason:          Option<CleanupReason>,
    ui:              Ui,
    ui_event:        bool,
    blocked:         bool,
    ack:             bool,
    waiter:          Option<ThreadId>,
    acknowledgement: super::ack::Receipt
}
impl State
{
    fn dormant(owner: OwnerRef) -> Self
    {
        Self { plan:            None,
               admitted:        false,
               result:          None,
               reason:          None,
               ui:              Ui::BeforeReply,
               ui_event:        false,
               blocked:         false,
               ack:             false,
               waiter:          None,
               acknowledgement: super::ack::Receipt::new(owner) }
    }
}
pub(crate) struct TrustedCloseHeader
{
    pub(super) canvas: super::canvas::CanvasState,
    origin:            Origin,
    notice:            Arc<Notice>,
    available:         Arc<Notice>,
    terminal:          AtomicBool,
    wake:              AtomicBool,
    state:             Spinlock<State>
}
/// Actual original bound endpoint; no issuer or numeric completion is added.
pub(crate) struct TrustedCloseHandle
{
    header:    Arc<TrustedCloseHeader>,
    id:        WindowId,
    recipient: u64,
    storage:   DomainId,
    waiter:    Option<ThreadId>,
    returned:  Returned
}
struct Returned
{
    notify: bool,
    notice: Arc<Notice>
}
impl Drop for Returned
{
    fn drop(&mut self)
    {
        if self.notify
        {
            // Handle fields have already released the actual header/readers.
            crate::kernel::window_sources_changed();
            self.notice.request();
        }
    }
}
impl Clone for TrustedCloseHandle
{
    fn clone(&self) -> Self
    {
        Self { header:    self.header.clone(),
               id:        self.id,
               recipient: self.recipient,
               storage:   self.storage,
               waiter:    self.waiter,
               returned:  Returned { notify: false,
                                     notice: self.header.notice.clone() } }
    }
}
impl Drop for TrustedCloseHandle
{
    fn drop(&mut self)
    {
        // Temporary work-handle restoration is NOT progress. Only the final
        // completed endpoint reader returning to its original pool can wake
        // stock admission; ordinary last-header destruction is guard-free too.
        let count = Arc::strong_count(&self.header);
        let complete = self.header.with_state(|s| s.ack && s.ui == Ui::Done);
        let last = match &self.header.origin
        {
            Origin::System(_) => count == 2,
            Origin::Process { .. } => count == 1
        };
        self.returned.notify = complete && last;
    }
}
/// Preflighted idle pool header. No fabricated Window identity is stored here.
pub(crate) struct TrustedBinding(Arc<TrustedCloseHeader>);
impl TrustedCloseHeader
{
    pub(super) fn work_notice(&self) -> Arc<Notice>
    {
        self.notice.clone()
    }

    pub(super) fn canvas_return_notice(self: &Arc<Self>) -> Option<Arc<Notice>>
    {
        let count = Arc::strong_count(self);
        let last = match &self.origin
        {
            Origin::System(_) => count == 2,
            Origin::Process { .. } => count == 1
        };
        (last && self.with_state(|state| state.ack && state.ui == Ui::Done)).then(|| {
                                                                                self.notice.clone()
                                                                            })
    }

    fn with_state<R>(&self, operation: impl FnOnce(&mut State) -> R) -> R
    {
        InvocationGate::run(|| operation(&mut self.state.lock()))
    }

    fn prepare(origin: Origin, notice: Arc<Notice>) -> Result<Arc<Self>, Status>
    {
        let available = Notice::linked(notice.clone())?;
        let owner = match &origin
        {
            Origin::System(source) => source.owner().clone(),
            Origin::Process { owner,
                              .. } => owner.clone()
        };
        Arc::try_new(Self { canvas: super::canvas::CanvasState::new(),
                            origin,
                            notice,
                            available,
                            terminal: AtomicBool::new(false),
                            wake: AtomicBool::new(false),
                            state: Spinlock::new(State::dormant(owner)) }).map_err(|_| NO_MEMORY)
    }

    pub(crate) fn prepare_owner(owner: &OwnerRef, allocation: &AllocationAuthority,
                                notice: Arc<Notice>)
                                -> Result<Arc<Self>, Status>
    {
        if allocation.owner() != owner.owner()
        {
            return Err(ACCESS_DENIED);
        }
        crate::allocator::with_target(crate::allocator::Target::Owner {
            account: allocation.clone(), class: mac_kernel_model::policy::MemoryClass::Kernel
        }, || Self::prepare(Origin::Process { allocation: allocation.clone(), owner: owner.clone() }, notice))
    }

    pub(super) fn prepare_system(source: crate::kernel::SystemResources, notice: Arc<Notice>)
                                 -> Result<Arc<Self>, Status>
    {
        Self::prepare(Origin::System(source), notice)
    }

    pub(super) fn available(self: &Arc<Self>) -> bool
    {
        Arc::strong_count(self) == 1
        && self.with_state(|s| s.plan.is_none() || (s.ack && s.ui == Ui::Done))
    }

    pub(crate) fn prepare_binding(self: &Arc<Self>) -> Result<TrustedBinding, Status>
    {
        if Arc::strong_count(self) != 1
        {
            return Err(SERVICE_BUSY);
        }
        self.with_state(|state| {
                if state.plan.is_some() && (!state.ack || state.ui != Ui::Done)
                {
                    return Err(SERVICE_BUSY);
                }
                self.canvas.reset()?;
                *state = State::dormant(state.acknowledgement.owner().clone());
                self.terminal.store(false, Ordering::Release);
                self.wake.store(false, Ordering::Release);
                self.available.take();
                Ok(TrustedBinding(self.clone()))
            })
    }

    fn bind(self: &Arc<Self>, plan: TrustedClosePlan, waiter: Option<ThreadId>)
            -> Result<TrustedCloseHandle, Status>
    {
        let valid = match &self.origin
        {
            Origin::Process { allocation,
                              owner } =>
            {
                allocation.owner() == plan.storage && owner.owner() == plan.storage
            }
            Origin::System(source) =>
            {
                source.owner().owner() == plan.storage
                && matches!(plan.window.source, WindowSource::System { resources, .. }
                        if source.same_source(resources))
            }
        };
        if !valid
        {
            return Err(ACCESS_DENIED);
        }
        self.with_state(|state| {
                if state.plan.is_some()
                {
                    return Err(SERVICE_BUSY);
                }
                state.plan = Some(plan);
                state.waiter = waiter;
                Ok(TrustedCloseHandle { header: self.clone(),
                                        id: plan.window.id,
                                        recipient: plan.window.owner,
                                        storage: plan.storage,
                                        waiter,
                                        returned: Returned { notify: false,
                                                             notice: self.notice.clone() } })
            })
    }

    fn publish_terminal(&self, state: &State)
    {
        if state.ui == Ui::Done && state.result.is_some_and(|result| result.is_err() || state.ack)
        {
            self.terminal.store(true, Ordering::Release);
            self.wake.store(true, Ordering::Release);
        }
    }
}
impl TrustedBinding
{
    pub(super) fn bind(self, window: Window, storage: DomainId, revision: u64)
                       -> Result<TrustedCloseHandle, Status>
    {
        self.0.bind(TrustedClosePlan { window,
                                       storage,
                                       revision },
                    None)
    }
}
impl TrustedCloseHandle
{
    pub(super) fn canvas_guardian(&self) -> super::canvas::WindowCanvasGuardian
    {
        super::canvas::WindowCanvasGuardian::trusted(self.header.clone(), self.id, self.storage)
    }

    pub(crate) fn window_id(&self) -> WindowId
    {
        self.id
    }

    pub(crate) fn owner(&self) -> u64
    {
        self.recipient
    }

    pub(crate) fn storage_owner(&self) -> DomainId
    {
        self.storage
    }

    pub(crate) fn waiter(&self) -> Option<ThreadId>
    {
        self.waiter
    }

    pub(crate) fn join_waiter(mut self, waiter: Option<ThreadId>) -> Result<Self, Status>
    {
        if let Some(waiter) = waiter
        {
            self.header.with_state(|state| {
                            if state.waiter.is_some_and(|actual| actual != waiter)
                            {
                                return Err(SERVICE_BUSY);
                            }
                            state.waiter = Some(waiter);
                            Ok(())
                        })?;
            self.waiter = Some(waiter);
        }
        Ok(self)
    }

    pub(crate) fn ready(&self) -> bool
    {
        // Result/UI/ACK edges share this child. They do not by themselves
        // authorize another attempt at the same retained author refusal.
        self.header.available.ready() && self.header.with_state(|state| !state.blocked)
    }

    pub(crate) fn wait_ready(&self) -> bool
    {
        self.header.terminal.load(Ordering::Acquire)
    }

    pub(crate) fn poll_result(&self) -> Option<Result<WindowId, CloseError>>
    {
        if !self.wait_ready()
        {
            return None;
        }
        self.header.with_state(|state| state.result)
    }

    pub(crate) fn deliver_wake(&self)
    {
        if self.header.wake.swap(false, Ordering::AcqRel)
           && let Some(waiter) = self.header.with_state(|s| s.waiter)
        {
            crate::machine::scheduler::callbacks::wake_waiter(waiter);
        }
    }

    pub(crate) fn arm_wait(&self)
    {
        self.header.with_state(|state| {
                       state.blocked = true;
                       self.header.available.take();
                   });
    }

    pub(super) fn changed(&self) -> bool
    {
        let changed = self.header.with_state(|state| {
                                     if state.ui == Ui::Claimed
                                     {
                                         state.ui_event = true;
                                     }
                                     let mut changed = core::mem::take(&mut state.blocked);
                                     if state.ui == Ui::Blocked
                                     {
                                         state.ui = Ui::Pending;
                                         changed = true;
                                     }
                                     changed
                                 });
        if changed
        {
            self.header.available.request();
        }
        changed
    }

    pub(super) fn refresh_ack(&self) -> bool
    {
        let changed = self.header.with_state(|s| {
                                     let changed = !s.ack && s.acknowledgement.refresh();
                                     if changed
                                     {
                                         s.ack = true;
                                         self.header.publish_terminal(s);
                                     }
                                     changed
                                 });
        if changed
        {
            self.header.available.request();
        }
        self.header.with_state(|s| s.ack)
    }

    pub(super) fn acknowledgement_done(&self) -> bool
    {
        self.header.with_state(|s| s.ack)
    }

    pub(super) fn ui_done(&self) -> bool
    {
        self.header.with_state(|state| state.ui == Ui::Done)
    }

    pub(super) fn resource_released(&self)
    {
        self.header.with_state(|state| state.acknowledgement.destroyed());
        self.refresh_ack();
        self.header.available.request();
        self.deliver_wake();
    }

    pub(crate) fn take_close_cleanup(&self) -> Option<CloseCleanup>
    {
        self.header.with_state(|state| {
                       if state.ui != Ui::Pending
                       {
                           return None;
                       }
                       // The retained header owns the original typed reason.
                       state.reason?;
                       state.ui = Ui::Claimed;
                       Some(CloseCleanup::Trusted(TrustedCleanup { handle:   self.clone(),
                                                                   returned: false }))
                   })
    }
}
pub(crate) struct TrustedCleanup
{
    handle:   TrustedCloseHandle,
    returned: bool
}
impl TrustedCleanup
{
    pub(crate) fn window_id(&self) -> WindowId
    {
        self.handle.id
    }

    pub(crate) fn owner(&self) -> u64
    {
        self.handle.recipient
    }

    pub(crate) fn storage_owner(&self) -> DomainId
    {
        self.handle.storage
    }

    pub(crate) fn arm_wait(&self)
    {
        self.handle.header.with_state(|s| {
                              s.ui_event = false;
                              self.handle.header.available.take();
                          });
    }

    pub(crate) fn defer(mut self)
    {
        let changed = self.handle.header.with_state(|s| {
                                            let changed = core::mem::take(&mut s.ui_event);
                                            s.ui = if changed { Ui::Pending } else { Ui::Blocked };
                                            changed
                                        });
        self.returned = true;
        if changed
        {
            self.handle.header.available.request();
        }
    }

    pub(crate) fn finish(mut self)
    {
        self.handle.header.with_state(|s| {
                              s.ui = Ui::Done;
                              self.handle.header.publish_terminal(s);
                          });
        self.returned = true;
        self.handle.header.available.request();
        self.handle.deliver_wake();
    }
}
impl Drop for TrustedCleanup
{
    fn drop(&mut self)
    {
        if !self.returned
        {
            self.handle.header.with_state(|s| {
                                  s.ui = Ui::Pending;
                              });
            self.handle.header.available.request();
        }
    }
}

impl WindowManager
{
    pub(crate) fn plan_trusted_close(&self, id: WindowId)
                                     -> Result<Option<TrustedClosePlan>, Status>
    {
        let (storage, owned) = self.windows
                                   .get_with_owner(id.0)
                                   .filter(|(_, owned)| owned.initialized)
                                   .ok_or(NOT_FOUND)?;
        if !owned.binding.matches(owned.entry)
        {
            return Err(BAD_ABI);
        }
        if crate::toolbox::resources::definitions::is_user(owned.entry)?
        {
            return Ok(None);
        }
        Ok(Some(TrustedClosePlan { window: owned.window,
                                   storage,
                                   revision: owned.revision.ok_or(BAD_ABI)? }))
    }

    pub(crate) fn trusted_close_stock(&self, plan: &TrustedClosePlan, waiter: Option<ThreadId>)
                                      -> Result<Option<TrustedCloseHandle>, Status>
    {
        self.validate_trusted_plan(plan)?;
        let Some(handle) =
            self.windows.get(plan.window.id.0).and_then(|owned| owned.trusted_close.as_ref())
        else
        {
            return Ok(None);
        };
        let mut handle = handle.clone();
        handle.waiter = waiter;
        Ok(Some(handle))
    }

    fn validate_trusted_plan(&self, plan: &TrustedClosePlan) -> Result<(), Status>
    {
        let (owner, owned) = self.windows
                                 .get_with_owner(plan.window.id.0)
                                 .filter(|(_, owned)| owned.initialized)
                                 .ok_or(NOT_FOUND)?;
        if owner != plan.storage
           || owned.revision != Some(plan.revision)
           || !owned.binding.matches(plan.window.entry)
        {
            return Err(SERVICE_BUSY);
        }
        if crate::toolbox::resources::definitions::is_user(owned.entry)?
        {
            return Err(ACCESS_DENIED);
        }
        Ok(())
    }

    pub(crate) fn admit_trusted_close(&mut self, plan: TrustedClosePlan,
                                      handle: &TrustedCloseHandle, events: &mut EventManager,
                                      when: u32)
                                      -> Result<WindowId, Status>
    {
        self.validate_trusted_plan(&plan)?;
        if handle.id != plan.window.id
           || handle.storage != plan.storage
           || !Arc::ptr_eq(&handle.header.notice, &self.work_notice)
        {
            return Err(ACCESS_DENIED);
        }
        if self.removals.len() == self.removals.capacity()
        {
            return Err(NO_MEMORY);
        }
        let id = plan.window.id;
        let active = self.active == Some(id);
        let ticket = handle.header.with_state(|state| {
                                       if state.admitted
                                          || state.plan.is_none_or(|bound| {
                                                           bound.window.id != id
                                                           || bound.storage != plan.storage
                                                       })
                                       {
                                           return Err(SERVICE_BUSY);
                                       }
                                       let owned = self.windows.get_mut(id.0).ok_or(NOT_FOUND)?;
                                       if let Some(stock) = &owned.trusted_close
                                       {
                                           if !Arc::ptr_eq(&stock.header, &handle.header)
                                           {
                                               return Err(ACCESS_DENIED);
                                           }
                                       }
                                       else
                                       {
                                           owned.trusted_close = Some(handle.clone());
                                       }
                                       handle.header.canvas.preflight_close()?;
                                       let ticket =
                                           self.windows.begin_release(id.0).ok_or(NOT_FOUND)?;
                                       handle.header.canvas.closed();
                                       state.plan = Some(plan);
                                       state.admitted = true;
                                       state.acknowledgement.bind(ticket.key());
                                       state.waiter = handle.waiter;
                                       Ok(ticket)
                                   })?;
        self.retire_window_ui(&plan.window);
        self.removals.push_back(operations::RemovalDebt::Trusted { ticket,
                                                                   handle: handle.clone() });
        if active
        {
            self.active = self.front_window();
            self.refresh_active(None);
            Self::activation(self.active, true, events, when);
        }
        handle.header.available.request();
        Ok(id)
    }

    /// The original owner-walk ticket remains canonical; no Context owns it.
    pub(super) fn defer_trusted_release(&mut self, ticket: ReleasedResource)
                                        -> Result<WindowId, Failure>
    {
        let Some(owned) = self.windows.releasing(&ticket)
        else
        {
            return Err(Failure::registration_mismatch(ticket));
        };
        let Some(handle) = owned.trusted_close.as_ref().cloned()
        else
        {
            return Err(Failure::window_retirement_storage(ticket,
                                                          self.removals.len(),
                                                          self.removals.capacity()));
        };
        if self.removals.len() == self.removals.capacity()
        {
            return Err(Failure::window_retirement_storage(ticket,
                                                          self.removals.len(),
                                                          self.removals.capacity()));
        }
        let Some(revision) = owned.revision
        else
        {
            return Err(Failure::window_definition(ticket,
                                                  owned.id.0,
                                                  owned.procedure,
                                                  i64::from(BAD_ABI)));
        };
        let plan = TrustedClosePlan { window: owned.window,
                                      storage: ticket.key().owner(),
                                      revision };
        let valid = handle.header.with_state(|s| {
                                     if s.admitted
                                        || handle.id != plan.window.id
                                        || handle.storage != plan.storage
                                        || !owned.binding.matches(plan.window.entry)
                                     {
                                         return false;
                                     }
                                     s.plan = Some(plan);
                                     s.acknowledgement.bind(ticket.key());
                                     s.admitted = true;
                                     true
                                 });
        if !valid
        {
            return Err(Failure::registration_mismatch(ticket));
        }
        self.retire_window_ui(&plan.window);
        let available = handle.header.available.clone();
        self.removals.push_back(operations::RemovalDebt::Trusted { ticket,
                                                                   handle });
        // The pump checks this original header's child before advancing it.
        // Request after canonical ticket custody; the linked notice also wakes
        // the common WM pump, just as explicit trusted-close admission does.
        available.request();
        Ok(plan.window.id)
    }

    pub(crate) fn trusted_close_work(&self, index: usize) -> Option<TrustedCloseHandle>
    {
        match self.removals.get(index)?
        {
            operations::RemovalDebt::Trusted { handle,
                                               .. } => Some(handle.clone()),
            operations::RemovalDebt::Completed(removed) => removed.trusted_handle(),
            operations::RemovalDebt::Acknowledgement { trusted,
                                                       .. } => trusted.clone(),
            _ => None
        }
    }

    pub(crate) fn trusted_removal_window(&self, handle: &TrustedCloseHandle)
                                         -> Result<Window, Status>
    {
        let (ticket, actual) = self.removals
                                   .iter()
                                   .find_map(|debt| {
                                       match debt
                                       {
                                           operations::RemovalDebt::Trusted { ticket,
                                                                              handle: actual }
                                               if Arc::ptr_eq(&actual.header, &handle.header) =>
                                           {
                                               Some((ticket, actual))
                                           }
                                           _ => None
                                       }
                                   })
                                   .ok_or(NOT_FOUND)?;
        let owned = self.windows.releasing(ticket).ok_or(NOT_FOUND)?;
        let plan = handle.header.with_state(|s| s.plan).ok_or(BAD_ABI)?;
        if actual.id != owned.id
           || ticket.key().owner() != handle.storage
           || owned.revision != Some(plan.revision)
           || !owned.binding.matches(plan.window.entry)
        {
            return Err(ACCESS_DENIED);
        }
        Ok(owned.window)
    }

    /// Call only after actual CTRL request_window_disposal has returned Ok.
    /// The trusted author runs through the original direct WDEF, never a User
    /// broker packet. Its mutation/positive result are retained on Store refusal.
    pub(crate) fn finish_trusted_close(&mut self, handle: &TrustedCloseHandle,
                                       children: &crate::toolbox::controls::objects::WindowRemoval<'_>)
                                       -> Result<WindowId, CloseError>
    {
        use mac_object_table::registry::CleanupFailure;
        if children.window_id() != handle.id.0
        {
            return Err(CloseError::NotFound(handle.id));
        }
        self.trusted_removal_window(handle).map_err(|_| CloseError::NotFound(handle.id))?;
        let index = self.removals
                        .iter()
                        .position(|debt| {
                            matches!(debt,
            operations::RemovalDebt::Trusted { handle: actual, .. }
                if Arc::ptr_eq(&actual.header, &handle.header))
                        })
                        .ok_or(CloseError::NotFound(handle.id))?;
        let (ticket, actual) = match self.removals.remove(index)
        {
            Some(operations::RemovalDebt::Trusted { ticket,
                                                    handle }) => (ticket, handle),
            Some(other) =>
            {
                self.removals.insert(index, other);
                return Err(CloseError::NotFound(handle.id));
            }
            None => return Err(CloseError::NotFound(handle.id))
        };
        let result = self.release_trusted_now(ticket, Some(actual.clone()));
        match result
        {
            Ok(id) =>
            {
                handle.header.with_state(|s| {
                                 if s.result.is_none()
                                 {
                                     s.result = Some(Ok(id));
                                 }
                                 if s.ui == Ui::BeforeReply
                                 {
                                     s.ui = Ui::Pending;
                                     s.reason = Some(CleanupReason::Author(0));
                                 }
                                 s.blocked = false;
                             });
                handle.header.available.request();
                Ok(id)
            }
            Err(failure) =>
            {
                let (ticket, cause) = failure.into_parts();
                let author = match cause
                {
                    crate::toolbox::handles::cleanup::Cause::WindowDefinitionDisposal { reply,
                                                                                        .. } =>
                    {
                        Some(reply)
                    }
                    _ => None
                };
                let changed =
                    handle.header.with_state(|s| {
                                     s.blocked = true;
                                     let mut changed = false;
                                     if let Some(reply) = author
                                     {
                                         if s.result.is_none()
                                         {
                                             s.result = Some(Err(CloseError::Cleanup(cause)));
                                             changed = true;
                                         }
                                         if s.ui == Ui::BeforeReply
                                         {
                                             s.ui = Ui::Pending;
                                             s.reason = Some(CleanupReason::Author(reply));
                                             changed = true;
                                         }
                                     }
                                     changed
                                 });
                self.removals.insert(index,
                                     operations::RemovalDebt::Trusted { ticket,
                                                                        handle: actual });
                if changed
                {
                    handle.header.available.request();
                }
                Err(CloseError::Cleanup(cause))
            }
        }
    }
}

impl WindowManager
{
    pub(crate) fn trusted_close_lookup(&self, id: WindowId) -> Option<TrustedCloseHandle>
    {
        self.removals.iter().find_map(|debt| {
                                match debt
                                {
                                    operations::RemovalDebt::Trusted { handle,
                                                                       .. } if handle.id == id =>
                                    {
                                        Some(handle.clone())
                                    }
                                    operations::RemovalDebt::Completed(removed) =>
                                    {
                                        removed.trusted_for(id)
                                    }
                                    operations::RemovalDebt::Acknowledgement { trusted,
                                                                               .. } =>
                                    {
                                        trusted.as_ref().filter(|handle| handle.id == id).cloned()
                                    }
                                    _ => None
                                }
                            })
    }

    pub(crate) fn trusted_close_accessible(&self, handle: &TrustedCloseHandle, recipient: u64)
                                           -> bool
    {
        if handle.recipient != recipient
        {
            return false;
        }
        if handle.storage.kind() != mac_object_table::domain::DomainKind::System
        {
            return handle.storage.incarnation() == recipient;
        }
        if let Some(activity) = crate::context::recovery_activity()
        {
            if handle.storage != activity.storage_owner().owner()
            {
                return false;
            }
            // Observe an already completed original close; this grants no new
            // Window effect or publication after its live slot has retired.
            if handle.acknowledgement_done() && handle.ui_done()
            {
                return true;
            }
            return self.recovery_windows.iter().flatten().any(|held| {
                                                             held.id == handle.id
                                                             && held.activity
                                                                    .same_session(&activity)
                                                         });
        }
        recipient == 0
        && matches!(&handle.header.origin, Origin::System(source)
            if source.owner().owner() == handle.storage)
    }

    pub(crate) fn releasing_has_trusted_close(&self, ticket: &ReleasedResource) -> bool
    {
        self.windows.releasing(ticket).is_some_and(|owned| owned.trusted_close.is_some())
    }
}

impl WindowManager
{
    /// Actual original WDEF source, not ambient selection or a caller flag.
    pub(crate) fn close_waits_on(&self, id: WindowId,
                                 execution: &crate::machine::scheduler::callbacks::CallbackExecution)
                                 -> Result<bool, Status>
    {
        let owned = self.windows.get(id.0).filter(|owned| owned.initialized).ok_or(NOT_FOUND)?;
        if !owned.binding.matches(owned.entry)
        {
            return Err(BAD_ABI);
        }
        Ok(owned.binding.callback_waits_on(execution))
    }
}
