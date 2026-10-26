//! Registered job payloads. The queue owns publication receipts; ordinary IDs
//! and waiter labels never become authority for a late completion.
use alloc::sync::Arc;

use mac_object_table::{domain::ReleasedResource,
                       projected,
                       registry::{Source, Specification}};
use spinning_top::{Spinlock, SpinlockGuard};

use super::*;
use crate::toolbox::handles::{CapacityPolicy, InvocationGate, KindCode, KindLease,
                              cleanup::Failure};

type Value = Arc<Spinlock<Record>>;
struct Pending
{
    wire:     u32,
    ticket:   ReleasedResource,
    owner:    Owner,
    failure:  Option<ReturnCause>,
    reported: bool
}
pub(super) type Guard<'a> = SpinlockGuard<'a, Record>;

#[derive(Clone, Copy)]
pub(super) enum Access
{
    Ordinary(u32),
    Completion(projected::CompletionKey)
}
impl From<u32> for Access
{
    fn from(id: u32) -> Self
    {
        Self::Ordinary(id)
    }
}
impl Access
{
    pub(super) fn id(self) -> u32
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
    store:      Option<projected::Store<Value, InvocationGate>>,
    receipts:   Vec<(u32, projected::CompletionKey)>,
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
            self.store = Some(projected::Store::new(Kind::DriverJobs, kinds.0, kinds.1)
                .map_err(crate::toolbox::handles::projection_status)?);
        }
        Ok(())
    }

    pub(super) fn insert(&mut self, subject: &subjects::Subject, value: Value)
                         -> Result<projected::Publication, Status>
    {
        self.receipts.try_reserve(1).map_err(|_| NO_MEMORY)?;
        self.retiring
            .try_reserve(self.receipts.len() + 1 - self.retiring.len())
            .map_err(|_| NO_MEMORY)?;
        let publication = self.store
                              .as_mut()
                              .ok_or(mac_abi::NOT_SUPPORTED)?
                              .insert_in_scope_with_receipt(subject.scope(), value)
                              .map_err(crate::toolbox::handles::projection_status)?;
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

    pub(super) fn get_mut(&self, id: u32) -> Option<Guard<'_>>
    {
        self.lookup(Access::Ordinary(id))
    }

    pub(super) fn receipt(&self, id: u32) -> Option<projected::CompletionKey>
    {
        self.receipts.iter().find(|(wire, _)| *wire == id).map(|(_, key)| *key)
    }

    pub(super) fn retained(&self, id: u32) -> Option<Guard<'_>>
    {
        self.lookup(Access::Completion(self.receipt(id)?))
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (u32, Guard<'_>)>
    {
        self.receipts
            .iter()
            .filter_map(|(id, key)| self.lookup(Access::Completion(*key)).map(|value| (*id, value)))
    }

    pub(super) fn remove(&mut self, id: u32)
    {
        let mark = if let Some(at) = self.retiring.iter().position(|pending| pending.wire == id)
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
            self.receipts.retain(|(wire, _)| *wire != id);
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
        Some(ReturnFailure { job: pending.wire,
                             owner: pending.owner,
                             cause })
    }

    pub(super) fn release(&mut self, ticket: ReleasedResource, storage: bool)
                          -> Result<Option<(Owner, projected::CompletionKey)>, Failure>
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
            self.receipts.retain(|(wire, _)| *wire != mark.wire());
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
        let wire = receipt.id();
        self.retiring.push(Pending { wire,
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
