//! Registered broker requests. Wire tickets correlate calls; continuation
//! authority is the retained publication receipt plus the same admitted scope.
//! No ordinary lookup can resurrect an invalidated request.
#![forbid(unsafe_code)]
use alloc::vec::Vec;
use core::cell::Cell;

use mac_object_table::{domain::{PoolError, ReleasedResource},
                       registry::{CleanupFailure, Release, Source, Specification},
                       store::CompletionKey};

use super::{Error, Request, Result};
use crate::{drivers::jobs::subjects::Subject,
            toolbox::handles::{CapacityPolicy, KindCode, KindLease, Store,
                               cleanup::{Failure, context::Context}}};

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
    id:      u64,
    value:   Cell<Request>,
    subject: Subject
}
struct Slot
{
    receipt:  CompletionKey,
    pending:  Option<ReleasedResource>,
    terminal: bool,
    failure:  Option<PoolError>
}
pub(super) struct Requests
{
    store:  Store<Record>,
    slots:  Vec<Slot>,
    // Derived serialized-provider occupancy, keyed by the facility-issued
    // interface ID. Admission/terminal ACK update it; input and polling do not.
    active: Vec<(u64, usize)>
}
impl Requests
{
    pub(super) fn new(kinds: (KindLease, KindLease)) -> Result<Self>
    {
        Ok(Self { store:  Store::owned(kinds.0, kinds.1).map_err(Error::Owner)?,
                  slots:  Vec::new(),
                  active: Vec::new() })
    }

    fn at(&self, id: u64) -> Option<usize>
    {
        self.slots.binary_search_by_key(&id, |slot| slot.receipt.id()).ok()
    }

    pub(super) fn insert(&mut self, subject: &Subject, value: Request) -> Result<u64>
    {
        self.slots.try_reserve(1).map_err(|_| Error::Exhausted)?;
        let counter = if value.offer.flags & mac_module::Offer::SERIALIZED != 0
        {
            let at = self.active.binary_search_by_key(&value.interface, |entry| entry.0);
            match at
            {
                Ok(at) =>
                {
                    self.active[at].1.checked_add(1).ok_or(Error::Exhausted)?;
                }
                Err(_) => self.active.try_reserve(1).map_err(|_| Error::Exhausted)?
            }
            Some(at)
        }
        else
        {
            None
        };
        let publication = self.store
                              .insert_in_scope_with_receipt(subject.scope(), |id| {
                                  Record { id,
                                           value: Cell::new(value),
                                           subject: subject.clone() }
                              })
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
                                 failure: None });
        Ok(id)
    }

    fn record(&self, access: Access<'_>) -> Result<&Record>
    {
        let at = self.at(access.id()).ok_or(Error::Stale)?;
        let slot = &self.slots[at];
        if slot.terminal
        {
            return Err(Error::Stale);
        }
        match access
        {
            Access::Ordinary(id) => self.store.get(id).ok_or(Error::Stale),
            Access::Continuing { subject,
                                 .. } =>
            {
                let record = self.store.completing(&slot.receipt).ok_or(Error::Stale)?;
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
        Ok(self.record(access)?.value.get())
    }

    pub(super) fn active(&self, access: Access<'_>, active: bool) -> Result<()>
    {
        let record = self.record(access)?;
        let mut value = record.value.get();
        value.active = active;
        record.value.set(value);
        Ok(())
    }

    /// Include invalidated and terminal-but-not-yet-dispatched records: their
    /// exact provider/interface pins remain until the real cleanup ACK.
    pub(super) fn iter(&self) -> impl Iterator<Item = (u64, Request)> + '_
    {
        self.slots.iter().filter_map(|slot| {
                             let record = self.store.completing(&slot.receipt)?;
                             Some((record.id, record.value.get()))
                         })
    }

    pub(super) fn finish(&mut self, access: Access<'_>) -> Result<()>
    {
        let request = self.record(access)?.value.get();
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
        self.retire(at);
        Ok(())
    }

    /// Called only after a terminal provider reply or an actual walk retry.
    /// A refused ticket stays in its preallocated slot with its typed cause.
    fn retire(&mut self, at: usize)
    {
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
        if mark.is_some()
        {
            self.slots.remove(at);
        }
        drop(mark);
    }

    pub(super) fn release(&mut self, ticket: ReleasedResource, storage: bool)
                          -> core::result::Result<(), Failure>
    {
        if storage
        {
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
        let id = match self.store.releasing(&ticket)
        {
            Some(record) => record.id,
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
