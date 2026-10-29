//! Registered broker requests. Wire tickets correlate calls; continuation
//! authority is the retained publication receipt plus the same admitted scope.
//! No ordinary lookup can resurrect an invalidated request.
#![forbid(unsafe_code)]
use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
use core::sync::atomic::Ordering;

use mac_object_table::{domain::{PoolError, ReleasedResource},
                       registry::{CleanupFailure, Release, Source, Specification},
                       shared::InvocationGate as _,
                       store::CompletionKey};

use super::{Error, Request, Result};
use crate::{drivers::jobs::subjects::Subject,
            toolbox::handles::{CapacityPolicy, InvocationGate, KindCode, KindLease, Store,
                               cleanup::{Failure, context::Context}}};

/// Trusted broker provenance, never accepted from a module's mutable Call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin
{
    Direct,
    Queued
}
pub(crate) mod cancellation;
pub(crate) mod dispatch;

pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"DRQS" } else { *b"DREQ" }),
                    label:    if storage { "Driver request storage" } else { "Driver requests" },
                    provider: "System Module Manager",
                    capacity: if storage
                    {
                        CapacityPolicy::HardBound { slots:  1,
                                                    reason: "One broker request backing per \
                                                             subject domain" }
                    }
                    else
                    {
                        CapacityPolicy::GrowableUntilMemory
                    },
                    source:   Source::Resident }
}

#[derive(Clone, Copy)]
pub(crate) enum Access<'a>
{
    Ordinary(u64),
    Continuing
    {
        ticket:  u64,
        subject: &'a Subject
    }
}
impl From<u64> for Access<'_>
{
    fn from(id: u64) -> Self
    {
        Self::Ordinary(id)
    }
}
impl Access<'_>
{
    fn id(self) -> u64
    {
        match self
        {
            Self::Ordinary(id) => id,
            Self::Continuing { ticket,
                               .. } => ticket
        }
    }
}
struct Record
{
    id:       u64,
    value:    Request,
    subject:  Subject,
    parent:   Option<CompletionKey>,
    dispatch: Arc<dispatch::Storage>
}
struct Slot
{
    receipt:      CompletionKey,
    pending:      Option<ReleasedResource>,
    terminal:     bool,
    failure:      Option<PoolError>,
    cancellation: Option<cancellation::Reservation>,
    reply:        Option<cancellation::Reply>,
    uncertain:    Option<mac_abi::Status>,
    // Last field: cancellation/reply backing dies before the Removed guard
    // destroys Record and performs the real canonical completion ACK.
    retired:      Option<mac_object_table::store::Removed<Record, InvocationGate>>
}

/// An actual Store removal guard, detached from Registry for destruction.
/// No new public ID or separate cleanup namespace is issued.
pub(crate) struct Retirement
{
    _slot: Slot
}
impl Slot
{
    fn uncertainty(&self) -> Option<mac_abi::Status>
    {
        self.uncertain.or_else(|| {
                          self.cancellation.as_ref().and_then(|reservation|
            (reservation.state.phase.load(Ordering::Acquire) == cancellation::UNCERTAIN)
                .then_some(mac_abi::ROLLBACK_FAILED))
                      })
    }
}
pub(super) struct Requests
{
    store:        Store<Record>,
    slots:        Vec<Slot>,
    // Derived serialized-provider occupancy, keyed by the facility-issued
    // interface ID. Admission/terminal ACK update it; input and polling do not.
    active:       Vec<(u64, usize)>,
    cancel_ready: VecDeque<CompletionKey>
}
impl Requests
{
    pub(super) fn new(kinds: (KindLease, KindLease)) -> Result<Self>
    {
        Ok(Self { store:        Store::owned(kinds.0, kinds.1).map_err(Error::Owner)?,
                  slots:        Vec::new(),
                  active:       Vec::new(),
                  cancel_ready: VecDeque::new() })
    }

    #[cfg(target_arch = "riscv64")]
    fn reserve_recovery(
        &mut self, owner: &crate::toolbox::handles::OwnerRef)
        -> Result<[mac_object_table::store::PublicationReservation<InvocationGate>; 2]>
    {
        let first = self.store.reserve_publication(owner).map_err(Error::Owner)?;
        let second = self.store.reserve_publication(owner).map_err(Error::Owner)?;
        let reserved = self.store.reserved_capacity();
        InvocationGate::with_system_allocation(|| {
            self.slots.try_reserve(reserved)?;
            self.active.try_reserve(reserved)
        }).map_err(|_| Error::Exhausted)?;
        Ok([first, second])
    }

    fn at(&self, id: u64) -> Option<usize>
    {
        self.slots.binary_search_by_key(&id, |slot| slot.receipt.id()).ok()
    }

    /// Synchronous resident calls authenticate the same nesting boundary but
    /// cannot publish requests or retain cancellation state.
    pub(super) fn validate_parent(&self, subject: &Subject, caller: u64, origin: Origin)
                                  -> Result<Option<CompletionKey>>
    {
        let parent = subject.current_request();
        if let Some(receipt) = parent
        {
            let record = self.store.completing(&receipt).ok_or(Error::RequestParent)?;
            let invoking = record.value;
            if origin != Origin::Direct
               || !record.dispatch.active()
               || invoking.module != caller
               || !record.subject.scope().same_scope(subject.scope())
            {
                return Err(Error::RequestParent);
            }
        }
        Ok(parent)
    }

    pub(super) fn insert(&mut self, subject: &Subject, value: Request,
                         preparation: &mut dispatch::Preparation)
                         -> Result<u64>
    {
        #[cfg(target_arch = "riscv64")]
        if let Some(activity) = subject.recovery_activity()
        {
            if subject.uses_protected_publication()
               && !(value.origin == Origin::Direct && subject.current_request().is_none())
            {
                return activity.with_storage(|| self.insert_prepared(subject, value, preparation));
            }
            let ordinary = subject.with_ordinary_publication();
            return crate::context::without_recovery(|| {
                self.insert_prepared(&ordinary, value, preparation)
            });
        }
        self.insert_prepared(subject, value, preparation)
    }

    fn insert_prepared(&mut self, subject: &Subject, value: Request,
                       preparation: &mut dispatch::Preparation)
                       -> Result<u64>
    {
        let parent = self.validate_parent(subject, value.caller, value.origin)?;
        let reserved = self.store.reserved_capacity();
        #[cfg(target_arch = "riscv64")]
        let activity = subject.recovery_activity();
        #[cfg(target_arch = "riscv64")]
        let recovery = subject.uses_protected_publication();
        #[cfg(not(target_arch = "riscv64"))]
        let recovery = false;
        if recovery
        {
            // Direct roots require a separate cancellation coordinator/control
            // buffer. This kit admits the owned queued root and its nested
            // direct leaf only; do not silently allocate an ordinary reserve.
            if value.origin == Origin::Direct && parent.is_none()
            {
                return Err(Error::RecoveryUnsupported);
            }
            if self.slots.len() == self.slots.capacity()
            {
                return Err(Error::Exhausted);
            }
        }
        else
        {
            InvocationGate::with_system_allocation(|| self.slots.try_reserve(1 + reserved))
                .map_err(|_| Error::Exhausted)?;
        }
        if value.origin == Origin::Direct && parent.is_none()
        {
            let needed = self.slots.len().checked_add(1).ok_or(Error::Exhausted)?;
            InvocationGate::with_system_allocation(|| {
                self.cancel_ready.try_reserve(needed.saturating_sub(self.cancel_ready.len()))
            }).map_err(|_| Error::Exhausted)?;
            if !preparation.cancellation_ready()
            {
                return Err(Error::State);
            }
        }
        else if preparation.cancellation_ready()
        {
            return Err(Error::RequestParent);
        }
        let counter = if value.offer.flags & mac_module::Offer::SERIALIZED != 0
        {
            let at = self.active.binary_search_by_key(&value.interface, |entry| entry.0);
            match at
            {
                Ok(at) =>
                {
                    self.active[at].1.checked_add(1).ok_or(Error::Exhausted)?;
                }
                Err(_) if recovery =>
                {
                    if self.active.len() == self.active.capacity()
                    {
                        return Err(Error::Exhausted);
                    }
                }
                Err(_) => InvocationGate::with_system_allocation(|| {
                              self.active.try_reserve(1 + reserved)
                          }).map_err(|_| Error::Exhausted)?
            }
            Some(at)
        }
        else
        {
            None
        };
        #[cfg(target_arch = "riscv64")]
        let pin = activity.map(crate::recovery::Activity::acknowledgement_pin);
        #[cfg(not(target_arch = "riscv64"))]
        let pin = None;
        let dispatch = preparation.storage().map_err(Error::AllocationScope)?.clone();
        let make = |id| {
            Record { id,
                     value,
                     subject: subject.clone(),
                     parent,
                     dispatch }
        };
        #[cfg(target_arch = "riscv64")]
        let publication = if let Some(activity) = activity.filter(|_| recovery)
        {
            activity.publication().with_request(subject, |reservation| {
                self.store.insert_reserved_in_scope_with_acknowledgement(reservation, subject.scope(), pin, make)
                    .map_err(|error| match error {
                        mac_object_table::store::PublicationError::Busy => Error::Busy,
                        mac_object_table::store::PublicationError::Pool(error) => Error::Owner(error)
                    })
            })?
        }
        else
        {
            self.store
                .insert_in_scope_with_acknowledgement(subject.scope(), pin, make)
                .map_err(Error::Owner)?
        };
        #[cfg(not(target_arch = "riscv64"))]
        let publication = self.store
                              .insert_in_scope_with_acknowledgement(subject.scope(), pin, make)
                              .map_err(Error::Owner)?;
        let (id, receipt) = publication.into_parts();
        if let Some(at) = counter
        {
            match at
            {
                Ok(at) => self.active[at].1 += 1,
                Err(at) => self.active.insert(at, (value.interface, 1))
            }
        }
        let at = self.slots.partition_point(|slot| slot.receipt.id() < id);
        self.slots.insert(at,
                          Slot { receipt,
                                 pending: None,
                                 terminal: false,
                                 failure: None,
                                 cancellation: preparation.take_cancellation(),
                                 reply: None,
                                 uncertain: None,
                                 retired: None });
        Ok(id)
    }

    fn record(&self, access: Access<'_>) -> Result<&Record>
    {
        let at = self.at(access.id()).ok_or(Error::Stale)?;
        let slot = &self.slots[at];
        match access
        {
            Access::Ordinary(_) if slot.terminal => Err(Error::Stale),
            Access::Ordinary(id) => self.store.get(id).ok_or(Error::Stale),
            Access::Continuing { subject,
                                 .. } =>
            {
                let record = self.store
                                 .completing(&slot.receipt)
                                 .or_else(|| {
                                     slot.retired
                                         .as_ref()
                                         .and_then(|removed| removed.completing(&slot.receipt))
                                 })
                                 .ok_or(Error::Stale)?;
                if !record.subject.scope().same_scope(subject.scope())
                {
                    return Err(Error::RequestSubject);
                }
                Ok(record)
            }
        }
    }

    pub(super) fn busy(&self, interface: u64) -> bool
    {
        self.active.binary_search_by_key(&interface, |entry| entry.0).is_ok()
    }

    pub(super) fn get(&self, access: Access<'_>) -> Result<Request>
    {
        Ok(self.record(access)?.value)
    }

    pub(super) fn terminal_reply(&self, access: Access<'_>) -> Result<bool>
    {
        Ok(self.record(access)?.dispatch.terminal())
    }

    pub(super) fn completed(&self, access: Access<'_>) -> Result<bool>
    {
        self.record(access)?;
        Ok(self.slots[self.at(access.id()).ok_or(Error::Stale)?].terminal)
    }

    pub(super) fn exclusive_dispatch(&self, access: Access<'_>) -> Result<bool>
    {
        Ok(Arc::strong_count(&self.record(access)?.dispatch) == 1)
    }

    pub(super) fn take_completed(&mut self, access: Access<'_>) -> Result<Retirement>
    {
        self.record(access)?;
        let at = self.at(access.id()).ok_or(Error::Stale)?;
        if self.slots[at].retired.is_none()
        {
            return Err(Error::Busy);
        }
        if !self.exclusive_dispatch(access)?
        {
            return Err(Error::Busy);
        }
        Ok(Retirement { _slot: self.slots.remove(at) })
    }

    pub(super) fn dispatch(&self, access: Access<'_>) -> Result<Arc<dispatch::Storage>>
    {
        Ok(self.record(access)?.dispatch.clone())
    }

    pub(super) fn invoking(&self, access: Access<'_>) -> Result<Subject>
    {
        let record = self.record(access)?;
        if !record.dispatch.active()
        {
            return Err(Error::RequestParent);
        }
        let receipt = self.slots[self.at(record.id).ok_or(Error::Stale)?].receipt;
        Ok(record.subject.in_request(receipt))
    }

    pub(super) fn is_active(&self, access: Access<'_>) -> Result<bool>
    {
        Ok(self.record(access)?.dispatch.active())
    }

    /// Include invalidated and terminal-but-not-yet-dispatched records: their
    /// exact provider/interface pins remain until the real cleanup ACK.
    pub(super) fn iter(&self) -> impl Iterator<Item = (u64, Request)> + '_
    {
        self.slots.iter().filter_map(|slot| {
                             let record = self.store.completing(&slot.receipt)
                                 .or_else(|| slot.retired.as_ref().and_then(|r| r.completing(&slot.receipt)))?;
                             Some((record.id, record.value))
                         })
    }

    pub(super) fn finish(&mut self, access: Access<'_>) -> Result<()>
    {
        let at = if self.completed(access)?
        {
            self.at(access.id()).ok_or(Error::Stale)?
        }
        else
        {
            self.mark_terminal(access)?
        };
        self.retire(at);
        Ok(())
    }

    fn mark_terminal(&mut self, access: Access<'_>) -> Result<usize>
    {
        let request = self.record(access)?.value;
        let at = self.at(access.id()).ok_or(Error::Stale)?;
        if request.offer.flags & mac_module::Offer::SERIALIZED != 0
        {
            let count = self.active
                            .binary_search_by_key(&request.interface, |entry| entry.0)
                            .map_err(|_| Error::State)?;
            let remaining = self.active[count].1.checked_sub(1).ok_or(Error::State)?;
            if remaining == 0
            {
                self.active.remove(count);
            }
            else
            {
                self.active[count].1 = remaining;
            }
        }
        self.slots[at].terminal = true;
        Ok(at)
    }

    /// Per-failure metadata work, not a per-input scan. Only direct roots of
    /// this exact module subject are ours; nested calls and owned queue work
    /// retain their existing cancellation coordinators.
    pub(super) fn cancel_entry(&mut self, module: u64, owner: mac_object_table::domain::DomainId)
    {
        for slot in &self.slots
        {
            if slot.terminal
            {
                continue;
            }
            let Some(record) = self.store.completing(&slot.receipt)
            else
            {
                continue;
            };
            let request = record.value;
            if record.parent.is_some()
               || request.caller != module
               || record.subject.scope().subject() != owner
            {
                continue;
            }
            let Some(reservation) = &slot.cancellation
            else
            {
                continue;
            };
            if reservation.state
                          .phase
                          .compare_exchange(cancellation::IDLE,
                                            cancellation::READY,
                                            Ordering::AcqRel,
                                            Ordering::Acquire)
                          .is_ok()
            {
                self.cancel_ready.push_back(slot.receipt);
            }
        }
    }

    pub(super) fn next_cancellation(&mut self) -> Result<Option<cancellation::Plan>>
    {
        let remaining = self.cancel_ready.len();
        for _ in 0..remaining
        {
            let Some(receipt) = self.cancel_ready.pop_front()
            else
            {
                break;
            };
            let Some(at) = self.at(receipt.id())
            else
            {
                continue;
            };
            let slot = &mut self.slots[at];
            if slot.terminal
            {
                continue;
            }
            let Some(record) = self.store.completing(&receipt)
            else
            {
                continue;
            };
            let request = record.value;
            if record.dispatch.active_or_subscribe()
            {
                self.cancel_ready.push_back(receipt);
                continue;
            }
            let reservation = slot.cancellation.as_mut().ok_or(Error::State)?;
            if reservation.state.phase.load(Ordering::Acquire) != cancellation::READY
            {
                continue;
            }
            let output = reservation.output.as_mut().ok_or(Error::State)?;
            if output.capacity() < request.max_reply as usize
            {
                return Err(Error::State);
            }
            output.resize(request.max_reply as usize, 0);
            let output = reservation.output.take().ok_or(Error::State)?;
            reservation.state.phase.store(cancellation::RUNNING, Ordering::Release);
            return Ok(Some(cancellation::Plan { request,
                                                subject: record.subject.clone(),
                                                receipt,
                                                state: reservation.state.clone(),
                                                active: true,
                                                output }));
        }
        Ok(None)
    }

    fn cancellation_record(&self, plan: &cancellation::Plan) -> Result<usize>
    {
        let record = self.store.completing(&plan.receipt).ok_or(Error::Stale)?;
        let at = self.at(plan.ticket()).ok_or(Error::Stale)?;
        let slot = &self.slots[at];
        let reservation = slot.cancellation.as_ref().ok_or(Error::RequestSubject)?;
        if slot.terminal
           || record.value != plan.request
           || !record.subject.scope().same_scope(plan.subject.scope())
           || !Arc::ptr_eq(&reservation.state, &plan.state)
           || plan.state.phase.load(Ordering::Acquire) != cancellation::RUNNING
        {
            return Err(Error::RequestSubject);
        }
        Ok(at)
    }

    pub(super) fn return_cancellation(&mut self, mut plan: cancellation::Plan)
                                      -> core::result::Result<(), cancellation::Refusal>
    {
        let at = match self.cancellation_record(&plan)
        {
            Ok(at) => at,
            Err(cause) =>
            {
                return Err(cancellation::Refusal { cause,
                                                   plan });
            }
        };
        // Validated above under this exclusive metadata borrow; no callback or
        // fallible publication occurs between the checks and the handback.
        let Some(reservation) = self.slots[at].cancellation.as_mut()
        else
        {
            return Err(cancellation::Refusal { cause: Error::State,
                                               plan });
        };
        plan.output.clear();
        reservation.output = Some(core::mem::take(&mut plan.output));
        plan.state.phase.store(cancellation::READY, Ordering::Release);
        self.cancel_ready.push_back(plan.receipt);
        plan.active = false;
        Ok(())
    }

    pub(super) fn acknowledge_cancellation(
        &mut self, mut plan: cancellation::Plan, status: mac_abi::Status, length: usize)
        -> core::result::Result<cancellation::Acknowledged, cancellation::Refusal>
    {
        if let Err(cause) = self.cancellation_record(&plan)
        {
            return Err(cancellation::Refusal { cause,
                                               plan });
        }
        if status > 0 || length > plan.output.len() || length > plan.request.max_reply as usize
        {
            return Err(cancellation::Refusal { cause:
                                                   Error::Contract(mac_module::Error::Limit),
                                               plan });
        }
        let access = Access::Continuing { ticket:  plan.ticket(),
                                          subject: &plan.subject };
        let at = match self.mark_terminal(access)
        {
            Ok(at) => at,
            Err(cause) =>
            {
                return Err(cancellation::Refusal { cause,
                                                   plan });
            }
        };
        plan.state.phase.store(cancellation::ACKNOWLEDGED, Ordering::Release);
        plan.active = false;
        if length != 0
        {
            plan.output.truncate(length);
            self.slots[at].reply = Some(cancellation::Reply { status,
                                                              bytes:
                                                                  core::mem::take(&mut plan.output) });
            Ok(cancellation::Acknowledged::ReplyRetained { bytes: length,
                                                           status })
        }
        else
        {
            // Return the preadmitted cancellation backing to Slot before its
            // owned retirement leaves Registry; do not destroy it under this
            // metadata borrow or before Removed's canonical completion ACK.
            if let Some(reservation) = self.slots[at].cancellation.as_mut()
            {
                plan.output.clear();
                reservation.output = Some(core::mem::take(&mut plan.output));
            }
            self.retire(at);
            Ok(cancellation::Acknowledged::Released)
        }
    }

    pub(super) fn retain_uncertain_cancellation(
        &mut self, mut plan: cancellation::Plan, status: mac_abi::Status)
        -> core::result::Result<(), cancellation::Refusal>
    {
        let Some(at) = self.at(plan.ticket())
        else
        {
            return Err(cancellation::Refusal { cause: Error::Stale,
                                               plan });
        };
        let Some(record) = self.store.completing(&plan.receipt)
        else
        {
            return Err(cancellation::Refusal { cause: Error::Stale,
                                               plan });
        };
        let Some(reservation) = self.slots[at].cancellation.as_mut()
        else
        {
            return Err(cancellation::Refusal { cause: Error::RequestSubject,
                                               plan });
        };
        if !record.subject.scope().same_scope(plan.subject.scope())
           || !Arc::ptr_eq(&reservation.state, &plan.state)
           || plan.state.phase.load(Ordering::Acquire) != cancellation::RUNNING
        {
            return Err(cancellation::Refusal { cause: Error::RequestSubject,
                                               plan });
        }
        reservation.output = Some(core::mem::take(&mut plan.output));
        plan.state.phase.store(cancellation::UNCERTAIN, Ordering::Release);
        plan.active = false;
        self.slots[at].uncertain = Some(status);
        Ok(())
    }

    /// Called only after a terminal provider reply or an actual walk retry.
    /// A refused ticket stays in its preallocated slot with its typed cause.
    fn retire(&mut self, at: usize)
    {
        if self.slots[at].reply.is_some() || self.slots[at].retired.is_some()
        {
            return;
        }
        let id = self.slots[at].receipt.id();
        let mark = if let Some(ticket) = self.slots[at].pending.take()
        {
            match self.store.release(ticket)
            {
                Ok(mark) => Some(mark),
                Err(error) =>
                {
                    let cause = error.cause();
                    #[cfg(target_arch = "riscv64")]
                    if self.slots[at].failure != Some(cause)
                    {
                        serial!("DRIVER_REQUEST_CLEANUP_REFUSED ticket={} cause={}", id, cause);
                    }
                    self.slots[at].failure = Some(cause);
                    self.slots[at].pending = Some(error.into_ticket());
                    None
                }
            }
        }
        else
        {
            self.store.remove(id)
        };
        if let Some(mark) = mark
        {
            self.slots[at].retired = Some(mark);
            #[cfg(target_arch = "riscv64")]
            crate::drivers::request_retirement_changed();
        }
    }

    /// Per-retirement work only. A live dispatch/preparation alias prevents
    /// physical disposal, so its canonical ACK cannot precede its actual data.
    pub(super) fn take_retirement(&mut self) -> (Option<Retirement>, bool)
    {
        let at =
            self.slots.iter().position(|slot| {
                                 slot.retired
                                     .as_ref()
                                     .and_then(|removed| removed.completing(&slot.receipt))
                                     .is_some_and(|record| Arc::strong_count(&record.dispatch) == 1)
                             });
        let retirement = at.map(|at| Retirement { _slot: self.slots.remove(at) });
        let pending = self.slots.iter().any(|slot| slot.retired.is_some());
        (retirement, pending)
    }

    pub(super) fn release(&mut self, ticket: ReleasedResource, storage: bool)
                          -> core::result::Result<(), Failure>
    {
        if storage
        {
            let owner = ticket.key().owner();
            if let Some((slot, record, status)) =
                self.slots.iter().find_map(|slot| {
                                     let record = self.store.completing(&slot.receipt)?;
                                     let status = slot.uncertainty()
                                                      .or_else(|| record.dispatch.uncertainty())?;
                                     (record.subject.scope().subject() == owner).then_some((slot,
                                                                                            record,
                                                                                            status))
                                 })
            {
                return Err(Failure::driver_cancellation_uncertain(ticket,
                                                                  slot.receipt.id(),
                                                                  record.value.caller,
                                                                  status));
            }
            if let Some((slot, reply, record)) =
                self.slots.iter().find_map(|slot| {
                                     let record = self.store.completing(&slot.receipt)?;
                                     if record.subject.scope().subject() != owner
                                     {
                                         return None;
                                     }
                                     Some((slot, slot.reply.as_ref()?, record))
                                 })
            {
                return Err(Failure::driver_reply_pending(ticket,
                                                         slot.receipt.id(),
                                                         record.value.caller,
                                                         reply.bytes.len(),
                                                         reply.status));
            }
            // Checkpoint owns the storage ticket until every retained payload
            // and its actual lower ACK settles. Never spin/poll on input.
            let mut at = self.slots.len();
            while at != 0
            {
                at -= 1;
                if self.slots[at].terminal && self.slots[at].pending.is_some()
                {
                    self.retire(at);
                }
            }
            return self.store.release_storage(ticket).map_err(Failure::from);
        }
        let (id, caller) = match self.store.releasing(&ticket)
        {
            Some(record) => (record.id, record.value.caller),
            None => return Err(Failure::registration_mismatch(ticket))
        };
        let Some(at) = self.at(id)
        else
        {
            return Err(Failure::registration_mismatch(ticket));
        };
        if self.slots[at].pending.is_some()
        {
            return Err(Failure::registration_mismatch(ticket));
        }
        if let Some(reply) = &self.slots[at].reply
        {
            return Err(Failure::driver_reply_pending(ticket,
                                                     id,
                                                     caller,
                                                     reply.bytes.len(),
                                                     reply.status));
        }
        if let Some(status) = self.slots[at].uncertainty().or_else(|| {
                                                              self.store
                                                .completing(&self.slots[at].receipt)
                                                .and_then(|record| record.dispatch.uncertainty())
                                                          })
        {
            return Err(Failure::driver_cancellation_uncertain(ticket, id, caller, status));
        }
        if let Some(record) = self.store.completing(&self.slots[at].receipt)
        {
            // This real owner-walk ticket may precede any Removed guard.
            // Its blocked lease handback must wake owner cleanup as well.
            let _ = record.dispatch.active_or_subscribe();
        }
        self.slots[at].pending = Some(ticket);
        if self.slots[at].terminal
        {
            self.retire(at);
        }
        // Nonterminal payload/backing remains in the store. Parent job
        // cancellation must obtain the provider's real terminal ACK; absence
        // keeps its domain quarantined rather than fabricating completion.
        Ok(())
    }
}

pub(crate) struct ReleaseRequests(pub bool);
impl<C: Context + ?Sized> Release<C, Failure> for ReleaseRequests
{
    fn release(&self, context: &mut C, ticket: ReleasedResource)
               -> core::result::Result<(), Failure>
    {
        if let Some(registry) = context.driver_registry()
        {
            return registry.requests.release(ticket, self.0);
        }
        #[cfg(target_arch = "riscv64")]
        {
            crate::drivers::release_request(ticket, self.0)
        }
        #[cfg(not(target_arch = "riscv64"))]
        {
            Err(Failure::context_unavailable(ticket, "driver request registry"))
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(target_arch = "riscv64")]
impl super::Registry
{
    pub(crate) fn reserve_recovery_requests(
        &mut self, owner: &crate::toolbox::handles::OwnerRef)
        -> Result<[mac_object_table::store::PublicationReservation<InvocationGate>; 2]>
    {
        self.requests.reserve_recovery(owner)
    }

    /// The first kit covers only the admitted shallow FSYS -> HDIR graph.
    /// Dynamic FORK/backing graphs need their own admitted endpoint bound.
    /// No driver identity/name substitutes for the real registry bindings.
    pub(crate) fn validate_recovery_graph(&self, interface: u64) -> Result<()>
    {
        let root = self.interface(interface)?;
        if root.offer.class != mac_module::ClassId(*b"FSYS") || root.module == 0
        {
            return Err(Error::RecoveryUnsupported);
        }
        let module = self.modules.get(root.module).ok_or(Error::Stale)?;
        if module.dependencies.len() != 1
           || !module.services.is_empty()
           || module.dependencies[0].class != mac_module::ClassId(*b"HDIR")
           || module.dependency_ids.len() != 1
        {
            return Err(Error::RecoveryUnsupported);
        }
        let binding = self.dependencies.get(module.dependency_ids[0]).ok_or(Error::Stale)?;
        if binding.dependent != root.module
           || binding.class != mac_module::ClassId(*b"HDIR")
           || binding.scope.is_some()
        {
            return Err(Error::RecoveryUnsupported);
        }
        let leaf = self.interface(binding.interface)?;
        if self.interfaces.key(binding.interface) != Some(binding.interface_key)
           || leaf.module != binding.provider
           || leaf.offer.class != binding.class
        {
            return Err(Error::Stale);
        }
        let leaf_module = self.modules.get(leaf.module).ok_or(Error::Stale)?;
        if !leaf_module.dependencies.is_empty()
           || !leaf_module.dependency_ids.is_empty()
           || leaf_module.services.iter().any(|service| {
                                             service.service != mac_module::ClassId(*b"VDEV")
                                             || service.version != 1
                                         })
           || self.dependencies.retained_values().any(|binding| {
                                                     (binding.dependent == root.module
                                                      || binding.dependent == leaf.module)
                                                     && binding.scope.is_some()
                                                 })
        {
            return Err(Error::RecoveryUnsupported);
        }
        Ok(())
    }
}
