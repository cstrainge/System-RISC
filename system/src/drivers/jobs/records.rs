//! Registered job payloads. The queue owns publication receipts; ordinary IDs
//! and waiter labels never become authority for a late completion.
use alloc::sync::Arc;

use mac_object_table::{domain::ReleasedResource,
                       registry::{Source, Specification},
                       shared::InvocationGate as _,
                       store};
use spinning_top::{Spinlock, SpinlockGuard};

use super::*;
use crate::toolbox::handles::{CapacityPolicy, InvocationGate, KindCode, KindLease,
                              cleanup::Failure};

type Value = Arc<Spinlock<Record>>;
struct Pending
{
    id:       u64,
    ticket:   ReleasedResource,
    owner:    Owner,
    failure:  Option<ReturnCause>,
    reported: bool
}
pub(super) type Guard<'a> = SpinlockGuard<'a, Record>;

#[derive(Clone, Copy)]
pub(super) enum Access
{
    Ordinary(u64),
    Completion(store::CompletionKey)
}
impl From<u64> for Access
{
    fn from(id: u64) -> Self
    {
        Self::Ordinary(id)
    }
}
impl Access
{
    pub(super) fn id(self) -> u64
    {
        match self
        {
            Self::Ordinary(id) => id,
            Self::Completion(receipt) => receipt.id()
        }
    }
}

pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"DJST" } else { *b"DJOB" }),
                    label:    if storage { "Driver job storage" } else { "Driver jobs" },
                    provider: "System I/O Manager",
                    source:   Source::Resident,
                    capacity: if storage
                    {
                        CapacityPolicy::HardBound { slots:  1,
                                                    reason: "One driver-job backing per subject \
                                                             domain" }
                    }
                    else
                    {
                        CapacityPolicy::GrowableUntilMemory
                    } }
}
pub(super) struct Records
{
    store:      Option<store::Store<Value, InvocationGate>>,
    receipts:   Vec<(u64, store::CompletionKey)>,
    retiring:   Vec<Pending>,
    unreported: usize
}
impl Records
{
    pub(super) const fn new() -> Self
    {
        Self { store:      None,
               receipts:   Vec::new(),
               retiring:   Vec::new(),
               unreported: 0 }
    }

    #[cfg(target_arch = "riscv64")]
    pub(super) fn prepared(&self) -> bool
    {
        self.store.is_some()
    }

    pub(super) fn prepare(&mut self, kinds: (KindLease, KindLease)) -> Result<(), Status>
    {
        if self.store.is_none()
        {
            self.store = Some(store::Store::owned(kinds.0, kinds.1).map_err(pool_status)?);
        }
        Ok(())
    }

    pub(super) fn reserved_capacity(&self) -> usize
    {
        self.store.as_ref().map_or(0, store::Store::reserved_capacity)
    }

    #[cfg(target_arch = "riscv64")]
    pub(super) fn reserve_publication(
        &mut self, owner: &crate::toolbox::handles::OwnerRef)
        -> Result<store::PublicationReservation<InvocationGate>, Status>
    {
        let reservation = self.store
                              .as_mut()
                              .ok_or(mac_abi::NOT_SUPPORTED)?
                              .reserve_publication(owner)
                              .map_err(pool_status)?;
        let reserved = self.reserved_capacity();
        InvocationGate::with_system_allocation(|| {
            self.receipts.try_reserve(reserved)?;
            self.retiring
                .try_reserve((self.receipts.len() + reserved).saturating_sub(self.retiring.len()))
        }).map_err(|_| NO_MEMORY)?;
        Ok(reservation)
    }

    pub(super) fn insert(&mut self, subject: &subjects::Subject, value: Value)
                         -> Result<store::Publication, Status>
    {
        let reserved = self.reserved_capacity();
        #[cfg(target_arch = "riscv64")]
        let activity = subject.recovery_activity();
        #[cfg(target_arch = "riscv64")]
        let recovery = subject.uses_protected_publication();
        #[cfg(not(target_arch = "riscv64"))]
        let recovery = false;
        if recovery
        {
            if self.receipts.len() == self.receipts.capacity()
               || self.retiring.capacity() < self.receipts.len() + 1
            {
                return Err(NO_MEMORY);
            }
        }
        else
        {
            InvocationGate::with_system_allocation(|| {
                self.receipts.try_reserve(1 + reserved)?;
                self.retiring
                    .try_reserve((self.receipts.len() + 1 + reserved).saturating_sub(self.retiring
                                                                                         .len()))
            }).map_err(|_| NO_MEMORY)?;
        }
        #[cfg(target_arch = "riscv64")]
        let pin = activity.map(crate::recovery::Activity::acknowledgement_pin);
        #[cfg(not(target_arch = "riscv64"))]
        let pin = None;
        let store = self.store.as_mut().ok_or(mac_abi::NOT_SUPPORTED)?;
        #[cfg(target_arch = "riscv64")]
        let publication = if let Some(activity) = activity.filter(|_| recovery)
        {
            activity.publication().with_job(subject, |reservation| {
                                       store.insert_reserved_in_scope_with_acknowledgement(reservation, subject.scope(), pin, |_| value)
                                            .map_err(super::recovery::publication_status)
                                   })?
        }
        else
        {
            store.insert_in_scope_with_acknowledgement(subject.scope(), pin, |_| value)
                 .map_err(pool_status)?
        };
        #[cfg(not(target_arch = "riscv64"))]
        let publication =
            store.insert_in_scope_with_acknowledgement(subject.scope(), pin, |_| value)
                 .map_err(pool_status)?;
        self.receipts.push(publication.into_parts());
        Ok(publication)
    }

    pub(super) fn lookup(&self, access: Access) -> Option<Guard<'_>>
    {
        let value = match access
        {
            Access::Ordinary(id) => self.store.as_ref()?.get(id),
            Access::Completion(receipt) => self.store.as_ref()?.completing(&receipt)
        }?;
        Some(value.lock())
    }

    pub(super) fn get_mut(&self, id: u64) -> Option<Guard<'_>>
    {
        self.lookup(Access::Ordinary(id))
    }

    pub(super) fn receipt(&self, id: u64) -> Option<store::CompletionKey>
    {
        self.receipts.binary_search_by_key(&id, |entry| entry.0).ok().map(|at| self.receipts[at].1)
    }

    #[cfg(test)]
    pub(super) fn key(&self, id: u64) -> Option<mac_object_table::domain::ResourceKey>
    {
        self.store.as_ref()?.key(id)
    }

    pub(super) fn len(&self) -> usize
    {
        self.receipts.len()
    }

    pub(super) fn after(&self, after: u64, owner: Owner) -> Option<store::CompletionKey>
    {
        let start = self.receipts.partition_point(|(id, _)| *id <= after);
        self.receipts[start..].iter().find_map(|(_, receipt)| {
                                         let record = self.lookup(Access::Completion(*receipt))?;
                                         (record.owner == owner).then_some(*receipt)
                                     })
    }

    pub(super) fn retained(&self, id: u64) -> Option<Guard<'_>>
    {
        self.lookup(Access::Completion(self.receipt(id)?))
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (u64, Guard<'_>)>
    {
        self.receipts
            .iter()
            .filter_map(|(id, key)| self.lookup(Access::Completion(*key)).map(|value| (*id, value)))
    }

    pub(super) fn remove(&mut self, id: u64)
    {
        let mark = if let Some(at) = self.retiring.iter().position(|pending| pending.id == id)
        {
            let pending = self.retiring.swap_remove(at);
            // The slot and its backing remained in the authoritative store
            // through actual I/O. Only this terminal path consumes the ticket.
            match self.store.as_mut()
            {
                Some(store) =>
                {
                    match store.release(pending.ticket)
                    {
                        Ok(mark) =>
                        {
                            if pending.failure.is_some() && !pending.reported
                            {
                                self.unreported -= 1;
                            }
                            Some(mark)
                        }
                        Err(error) =>
                        {
                            if pending.failure.is_none() || pending.reported
                            {
                                self.unreported += 1;
                            }
                            let cause = error.cause();
                            self.retiring
                                .push(Pending { ticket: error.into_ticket(),
                                                failure:
                                                    Some(ReturnCause::CleanupRefused(cause)),
                                                reported: false,
                                                ..pending });
                            None
                        }
                    }
                }
                None =>
                {
                    if pending.failure.is_none() || pending.reported
                    {
                        self.unreported += 1;
                    }
                    self.retiring
                        .push(Pending { failure: Some(ReturnCause::CleanupStoreMissing),
                                        reported: false,
                                        ..pending });
                    None
                }
            }
        }
        else
        {
            self.store.as_mut().and_then(|store| store.remove(id))
        };
        if mark.is_some()
        {
            if let Ok(at) = self.receipts.binary_search_by_key(&id, |entry| entry.0)
            {
                self.receipts.remove(at);
            }
        }
        drop(mark);
    }

    pub(super) fn has_failures(&self) -> bool
    {
        self.unreported != 0
    }

    pub(super) fn failure(&mut self) -> Option<ReturnFailure>
    {
        let pending = self.retiring
                          .iter_mut()
                          .find(|pending| pending.failure.is_some() && !pending.reported)?;
        let cause = pending.failure?;
        pending.reported = true;
        self.unreported -= 1;
        Some(ReturnFailure { job: pending.id,
                             owner: pending.owner,
                             cause })
    }

    pub(super) fn release(&mut self, ticket: ReleasedResource, storage: bool)
                          -> Result<Option<(Owner, store::CompletionKey)>, Failure>
    {
        let Some(store) = self.store.as_mut()
        else
        {
            return Err(Failure::context_unavailable(ticket, "admitted driver-job storage"));
        };
        if storage
        {
            return store.release_storage(ticket).map(|()| None).map_err(Failure::from);
        }
        // Every live publication reserves a retirement slot before callback
        // execution. A refused dispatch retains its exact ticket for retry.
        if self.retiring.len() == self.retiring.capacity()
        {
            return Err(Failure::job_retirement_storage(ticket,
                                                       self.retiring.len(),
                                                       self.retiring.capacity()));
        }
        let Some(record) = store.releasing(&ticket)
        else
        {
            return Err(Failure::context_unavailable(ticket, "matching admitted job payload"));
        };
        let data = record.lock();
        if matches!(data.state, State::Released)
        {
            drop(data);
            let mark = store.release(ticket).map_err(Failure::from)?;
            if let Ok(at) = self.receipts.binary_search_by_key(&mark.id(), |entry| entry.0)
            {
                self.receipts.remove(at);
            }
            drop(mark);
            return Ok(None);
        }
        let owner = data.owner;
        // Retaining the ticket leaves the value in its original backing.
        // Taking a removal guard here would detach that backing while other
        // scopes still need to admit Close/Abort after ordinary admission stops.
        let receipt =
            data.receipt
                .filter(|receipt| {
                    store.completing(receipt).is_some_and(|value| Arc::ptr_eq(value, record))
                });
        drop(data);
        let Some(receipt) = receipt
        else
        {
            return Err(Failure::context_unavailable(ticket, "original job publication receipt"));
        };
        let id = receipt.id();
        self.retiring.push(Pending { id,
                                     ticket,
                                     owner,
                                     failure: None,
                                     reported: false });
        Ok(Some((owner, receipt)))
    }
}

#[cfg(target_arch = "riscv64")]
pub(crate) struct ReleaseJobs(pub bool);
#[cfg(target_arch = "riscv64")]
impl
    mac_object_table::registry::Release<dyn crate::toolbox::handles::cleanup::context::Context,
                                        Failure> for ReleaseJobs
{
    fn release(&self, _: &mut (dyn crate::toolbox::handles::cleanup::context::Context + 'static),
               ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        crate::drivers::release_job(ticket, self.0)
    }
}

#[cfg(test)]
mod tests;

pub(super) fn pool_status(error: mac_object_table::domain::PoolError) -> Status
{
    crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error))
}
