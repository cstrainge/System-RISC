//! System-owned physical devices with ledger-owned claim aliases and DMA maps.
//! Only a positive hardware stop acknowledgement permits reclamation. The bus
//! adapter owns registers; this policy layer contains no hardware or unsafe code.
#![forbid(unsafe_code)]
use core::cell::Cell;

use mac_abi::{ACCESS_DENIED, BAD_ABI, BAD_ARGUMENT, NO_MEMORY, NOT_FOUND, SERVICE_BUSY, Status};
use mac_device_interface::{Description, Mapping};
use mac_object_table::{aliases, domain::ReleasedResource, registry::CleanupFailure,
                       store::CompletionKey};

use crate::toolbox::handles::{InvocationGate, KindLease, OwnerRef, Store, cleanup::Failure};
pub(crate) mod lifetime;

pub trait Bus
{
    fn probe(&mut self, ordinal: u32) -> Result<Description, Status>;
    fn request_reset(&mut self, ordinal: u32) -> Result<(), Status>;
    fn stopped(&mut self, ordinal: u32) -> Result<bool, Status>;
}
#[derive(PartialEq, Eq)]
struct Physical
{
    ordinal:     u32,
    description: Description
}
struct Claim
{
    owner:      OwnerRef,
    receipt:    Cell<Option<CompletionKey>>,
    maps:       Cell<usize>,
    reset:      Cell<Option<u64>>,
    cleanup:    Cell<bool>,
    reset_sent: Cell<bool>
}
struct Dma
{
    claim: CompletionKey
}
pub struct Devices<B>
{
    bus:    B,
    claims: aliases::Store<Physical, InvocationGate, Claim>,
    maps:   Store<Dma>
}
impl<B: Bus> Devices<B>
{
    pub(crate) fn new(bus: B, system: OwnerRef, kinds: [KindLease; 6]) -> Result<Self, Status>
    {
        let [alias, alias_storage, object, object_storage, map, map_storage] = kinds;
        Ok(Self { bus,
                  claims: aliases::Store::new(system,
                                              (alias, alias_storage),
                                              (object, object_storage)).map_err(alias_status)?,
                  maps: Store::owned(map, map_storage).map_err(pool_status)? })
    }

    pub fn probe(&mut self, ordinal: u32) -> Result<Description, Status>
    {
        self.bus.probe(ordinal)
    }

    /// The caller checked the exact bus/key against its admitted contract.
    /// A pending or quarantined claim remains exclusive, even after invalidation.
    pub(crate) fn claim(&mut self, owner: &OwnerRef, ordinal: u32, expected: Description)
                        -> Result<Description, Status>
    {
        if expected.handle != 0 || expected.flags & mac_device_interface::RESIDENT_OWNED != 0
        {
            return Err(ACCESS_DENIED);
        }
        if self.claims.retained().any(|(physical, _)| physical.ordinal == ordinal)
        {
            return Err(SERVICE_BUSY);
        }
        if self.bus.probe(ordinal)? != expected
        {
            return Err(NOT_FOUND);
        }
        let publication = self.claims
                              .insert_with_receipt(owner,
                                                   Physical { ordinal,
                                                              description: expected },
                                                   Claim { owner:      owner.clone(),
                                                           receipt:    Cell::new(None),
                                                           maps:       Cell::new(0),
                                                           reset:      Cell::new(None),
                                                           cleanup:    Cell::new(false),
                                                           reset_sent: Cell::new(false) })
                              .map_err(alias_status)?;
        let (id, receipt) = publication.into_parts();
        // Use the authenticated publication, including if a concurrent walk
        // invalidated admission immediately after its transaction committed.
        let (_, claim) = self.claims.completing(&receipt).ok_or(BAD_ABI)?;
        claim.receipt.set(Some(receipt));
        Ok(Description { handle: id,
                         ..expected })
    }

    /// The caller checked this range against its pinned module reservation.
    /// Physical page pinning is a later page/heap gate, not claimed by this map.
    pub(crate) fn map(&mut self, owner: &OwnerRef, device: u64, address: u64, length: u64)
                      -> Result<Mapping, Status>
    {
        let (physical, claim) = self.claims.get(owner, device).map_err(alias_status)?;
        if claim.reset.get().is_some() || claim.cleanup.get()
        {
            return Err(SERVICE_BUSY);
        }
        if length == 0 || address.checked_add(length).is_none()
        {
            return Err(BAD_ARGUMENT);
        }
        if physical.description.flags & mac_device_interface::COHERENT_DMA == 0
        {
            return Err(mac_abi::NOT_SUPPORTED);
        }
        let count = claim.maps.get().checked_add(1).ok_or(NO_MEMORY)?;
        let receipt = claim.receipt.get().ok_or(BAD_ABI)?;
        let publication = self.maps
                              .insert_with_receipt(owner, |_| Dma { claim: receipt })
                              .map_err(pool_status)?;
        claim.maps.set(count);
        Ok(Mapping { handle: publication.id(),
                     address,
                     length })
    }

    pub(crate) fn start_reset(&mut self, owner: &OwnerRef, device: u64, ticket: u64)
                              -> Result<bool, Status>
    {
        if ticket == 0
        {
            return Err(BAD_ARGUMENT);
        }
        let (physical, claim) = self.claims.get(owner, device).map_err(alias_status)?;
        if claim.cleanup.get()
        {
            return Err(SERVICE_BUSY);
        }
        if claim.reset.get().is_some()
        {
            return Err(SERVICE_BUSY);
        }
        claim.reset.set(Some(ticket));
        if let Err(error) = self.bus.request_reset(physical.ordinal)
        {
            claim.reset.set(None);
            return Err(error);
        }
        Self::reset_ack(&mut self.bus, physical, claim)
    }

    fn reset_ack(bus: &mut B, physical: &Physical, claim: &Claim) -> Result<bool, Status>
    {
        match bus.stopped(physical.ordinal)
        {
            Ok(true) =>
            {
                claim.reset.set(None);
                Ok(true)
            }
            Ok(false) => Ok(false),
            Err(error) =>
            {
                claim.reset.set(None);
                Err(error)
            }
        }
    }

    /// A genuine broker ticket continues only the reset that admitted it. No
    /// ordinary invalidated alias is revived when owner cleanup has begun.
    pub(crate) fn reset_pending(&mut self, owner: &OwnerRef, ticket: u64) -> Result<bool, Status>
    {
        let (physical, claim) = self.claims
                                    .retained()
                                    .find(|(_, claim)| {
                                        claim.owner.owner() == owner.owner()
                                        && claim.reset.get() == Some(ticket)
                                    })
                                    .ok_or(NOT_FOUND)?;
        Self::reset_ack(&mut self.bus, physical, claim)
    }

    fn owned_map(&self, owner: &OwnerRef, id: u64) -> Result<&Dma, Status>
    {
        let (actual, map) = self.maps.get_with_owner(id).ok_or(NOT_FOUND)?;
        if actual != owner.owner()
        {
            return Err(ACCESS_DENIED);
        }
        Ok(map)
    }

    pub(crate) fn unmap(&mut self, owner: &OwnerRef, mapping: u64) -> Result<(), Status>
    {
        let map = self.owned_map(owner, mapping)?;
        let (physical, claim) = self.claims.completing(&map.claim).ok_or(BAD_ABI)?;
        if claim.reset.get().is_some() || claim.cleanup.get()
        {
            return Err(SERVICE_BUSY);
        }
        if !self.bus.stopped(physical.ordinal)?
        {
            return Err(SERVICE_BUSY);
        }
        let count = claim.maps.get().checked_sub(1).ok_or(BAD_ABI)?;
        let removed = self.maps.remove(mapping).ok_or(NOT_FOUND)?;
        claim.maps.set(count);
        drop(removed);
        Ok(())
    }

    pub(crate) fn release(&mut self, owner: &OwnerRef, device: u64) -> Result<(), Status>
    {
        let (physical, claim) = self.claims.get(owner, device).map_err(alias_status)?;
        if claim.reset.get().is_some() || claim.maps.get() != 0 || claim.cleanup.get()
        {
            return Err(SERVICE_BUSY);
        }
        if !self.bus.stopped(physical.ordinal)?
        {
            return Err(SERVICE_BUSY);
        }
        self.claims.remove(owner, device).map_err(alias_status)
    }

    /// A reset write is attempted once on successful submission. A failed write
    /// is retriable, never evidence that DMA stopped. Each retry probes once;
    /// there is no busy-wait and no work on unrelated input events.
    fn stop(bus: &mut B, physical: &Physical, claim: &Claim) -> Result<bool, Status>
    {
        claim.cleanup.set(true);
        if !claim.reset_sent.get()
        {
            bus.request_reset(physical.ordinal)?;
            claim.reset_sent.set(true);
        }
        bus.stopped(physical.ordinal)
    }

    pub(crate) fn cleanup(&mut self, ticket: ReleasedResource, kind: lifetime::Registration)
                          -> Result<(), Failure>
    {
        use lifetime::Registration::*;
        match kind
        {
            Alias =>
            {
                let Some((physical, claim)) = self.claims.releasing(&ticket)
                else
                {
                    return Err(Failure::registration_mismatch(ticket));
                };
                let ordinal = physical.ordinal;
                let stopped = match Self::stop(&mut self.bus, physical, claim)
                {
                    Ok(stopped) => stopped,
                    Err(status) => return Err(Failure::device_stop(ticket, ordinal, status))
                };
                if !stopped || claim.maps.get() != 0
                {
                    return Err(Failure::device_pending(ticket,
                                                       ordinal,
                                                       stopped,
                                                       claim.maps.get(),
                                                       claim.reset.get()));
                }
                self.claims.release_alias(ticket).map_err(Failure::from)
            }
            Map =>
            {
                let Some(map) = self.maps.releasing(&ticket)
                else
                {
                    return Err(Failure::registration_mismatch(ticket));
                };
                let Some((physical, claim)) = self.claims.completing(&map.claim)
                else
                {
                    return Err(Failure::context_unavailable(ticket, "retained device claim"));
                };
                let ordinal = physical.ordinal;
                match Self::stop(&mut self.bus, physical, claim)
                {
                    Ok(true) => (),
                    Ok(false) =>
                    {
                        return Err(Failure::device_pending(ticket,
                                                           ordinal,
                                                           false,
                                                           claim.maps.get(),
                                                           claim.reset.get()));
                    }
                    Err(status) => return Err(Failure::device_stop(ticket, ordinal, status))
                }
                let Some(count) = claim.maps.get().checked_sub(1)
                else
                {
                    return Err(Failure::context_unavailable(ticket, "device mapping count"));
                };
                let removed = self.maps.release(ticket).map_err(Failure::from)?;
                claim.maps.set(count);
                drop(removed);
                Ok(())
            }
            AliasStorage => self.claims.release_alias_storage(ticket).map_err(Failure::from),
            Object => self.claims.release_object(ticket).map_err(Failure::from),
            ObjectStorage => self.claims.release_object_storage(ticket).map_err(Failure::from),
            MapStorage => self.maps.release_storage(ticket).map_err(Failure::from)
        }
    }
}
impl<B> Drop for Devices<B>
{
    fn drop(&mut self)
    {
        // No destructor is allowed to interpret losing its manager as a stop
        // ACK. Quarantine both layers before their normal store destructors.
        self.maps.quarantine_live();
        self.claims.quarantine_live();
    }
}
fn pool_status(error: mac_object_table::domain::PoolError) -> Status
{
    crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error))
}
fn alias_status(error: aliases::Error) -> Status
{
    match error
    {
        aliases::Error::Pool(error) => pool_status(error),
        aliases::Error::Missing { .. } | aliases::Error::TargetGone { .. } => NOT_FOUND,
        aliases::Error::Foreign { .. } => ACCESS_DENIED,
        aliases::Error::ReferenceLimit { .. } => NO_MEMORY,
        aliases::Error::NotSystem { .. } => BAD_ABI
    }
}
#[cfg(test)]
mod tests;
