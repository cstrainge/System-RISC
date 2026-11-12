//! System-owned physical devices with ledger-owned claim aliases and DMA maps.
//! Only a positive hardware stop acknowledgement permits reclamation. The bus
//! adapter owns registers; this policy layer contains no hardware or unsafe code.
#![forbid(unsafe_code)]
#[cfg(systemrisc)]
use alloc::sync::Arc;
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
    /// Prove a contiguous coherent bus span for pinned CPU storage. This is a
    /// pure address translation, not an allocation or hardware publication.
    /// CPU addresses must never be returned implicitly by manager policy.
    fn dma_address(&self, cpu_address: u64, bytes: u64) -> Result<u64, Status>;
    /// The address the device at `ordinal` uses for memory the CPU finds at
    /// `physical`. The machine states this per bus; a device that cannot
    /// reach the span is refused rather than handed an address that is not
    /// its own.
    fn device_address(&self, ordinal: u32, physical: u64, bytes: u64) -> Result<u64, Status>;
    /// Bring the CPU's cached copy of a span and memory into step, in the
    /// direction named. Called only for a device outside the CPU's caches.
    fn synchronize(&self, cpu_address: u64, bytes: u64, for_device: bool) -> Result<(), Status>;
    /// Where the CPU finds memory the firmware owns and shares with the device
    /// at `ordinal`. Refused for a span the machine does not declare as such.
    fn shared_view(&self, ordinal: u32, physical: u64, bytes: u64) -> Result<u64, Status>;
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
    #[cfg(systemrisc)]
    frame:      crate::images::ModuleFrame,
    owner:      OwnerRef,
    receipt:    Cell<Option<CompletionKey>>,
    maps:       Cell<usize>,
    reset:      Cell<Option<u64>>,
    cleanup:    Cell<bool>,
    reset_sent: Cell<bool>,
    #[cfg(systemrisc)]
    allocation: Cell<Option<(u64, CompletionKey)>>
}
struct Dma
{
    claim:  CompletionKey,
    /// The CPU span of a mapping or view, which SYNC works over. An
    /// allocation has none: its span is its buffer's.
    span:   Option<(u64, u64)>,
    #[cfg(systemrisc)]
    buffer: Option<Arc<crate::machine::dma::Buffer>>
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
    pub(crate) fn claim(&mut self, owner: &OwnerRef, ordinal: u32, expected: Description,
                        #[cfg(systemrisc)] frame: crate::images::ModuleFrame)
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
                                                   Claim { #[cfg(systemrisc)]
                                                           frame,
                                                           owner: owner.clone(),
                                                           receipt: Cell::new(None),
                                                           maps: Cell::new(0),
                                                           reset: Cell::new(None),
                                                           cleanup: Cell::new(false),
                                                           reset_sent: Cell::new(false),
                                                           #[cfg(systemrisc)]
                                                           allocation: Cell::new(None) })
                              .map_err(alias_status)?;
        let (id, receipt) = publication.into_parts();
        // Use the authenticated publication, including if a concurrent walk
        // invalidated admission immediately after its transaction committed.
        let (_, claim) = self.claims.completing(&receipt).ok_or(BAD_ABI)?;
        claim.receipt.set(Some(receipt));
        Ok(Description { handle: id,
                         ..expected })
    }

    /// The caller checked the original module's execution reservation. The
    /// native bus resolves its actual owned segment/PTEs and physical span.
    /// This DMA/claim ledger remains outside the image root kinds, preventing
    /// image retirement until device maps and positive stop/reset complete.
    pub(crate) fn map(&mut self, owner: &OwnerRef, device: u64, address: u64, length: u64,
                      synchronized: bool)
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
        // A device outside the CPU's caches is mapped only for a driver that
        // has said it synchronizes the mapping. Without that the two would
        // read each other's stale bytes on a board and agree under emulation,
        // so the mapping is refused here rather than discovered there.
        if !synchronized
           && physical.description.flags & mac_device_interface::COHERENT_DMA == 0
        {
            return Err(mac_abi::NOT_SUPPORTED);
        }
        let bus_address = self.bus.dma_address(address, length)?;
        let bus_address = self.bus.device_address(physical.ordinal, bus_address, length)?;
        if bus_address.checked_add(length).is_none()
        {
            return Err(BAD_ARGUMENT);
        }
        let count = claim.maps.get().checked_add(1).ok_or(NO_MEMORY)?;
        let receipt = claim.receipt.get().ok_or(BAD_ABI)?;
        let publication = self.maps
                              .insert_with_receipt(owner, |_| {
                                  Dma { claim:                                  receipt,
                                        span:                                   Some((address,
                                                                                      length)),
                                        #[cfg(systemrisc)]
                                        buffer:                                 None }
                              })
                              .map_err(pool_status)?;
        claim.maps.set(count);
        Ok(Mapping { handle: publication.id(),
                     address: bus_address,
                     length })
    }

    #[cfg(systemrisc)]
    pub(crate) fn start_allocate(&mut self, owner: &OwnerRef, device: u64, bytes: u64, ticket: u64,
                                 device_accessed: bool)
                                 -> Result<(), Status>
    {
        let (physical, claim) = self.claims.get(owner, device).map_err(alias_status)?;
        if ticket == 0 || bytes == 0
        {
            return Err(BAD_ARGUMENT);
        }
        if claim.reset.get().is_some() || claim.cleanup.get() || claim.allocation.get().is_some()
        {
            return Err(SERVICE_BUSY);
        }
        // What the device itself addresses must not sit behind a cache the
        // device does not share. Where the machine does not state coherence,
        // such a buffer comes from the uncached pool, and there is then nothing
        // for the driver to keep in step. A buffer only the CPU touches stays
        // ordinary cached memory on any device.
        let uncached =
            device_accessed && physical.description.flags & mac_device_interface::COHERENT_DMA == 0;
        let count = claim.maps.get().checked_add(1).ok_or(NO_MEMORY)?;
        let receipt = claim.receipt.get().ok_or(BAD_ABI)?;
        let buffer = if uncached
        {
            crate::machine::dma::Buffer::new_uncached(bytes)?
        }
        else
        {
            crate::machine::dma::Buffer::new(bytes)?
        };
        let buffer = Arc::try_new(buffer).map_err(|_| NO_MEMORY)?;
        // Prove the actual resident heap bus alias before storing any handle.
        buffer.device_address()?;
        let publication = self.maps
                              .insert_with_receipt(owner, |_| {
                                  Dma { claim:  receipt,
                                        span:   None,
                                        buffer: Some(buffer) }
                              })
                              .map_err(pool_status)?;
        claim.maps.set(count);
        claim.allocation.set(Some((ticket, publication.into_parts().1)));
        Ok(())
    }

    #[cfg(systemrisc)]
    pub(crate) fn advance_allocate(&mut self, owner: &OwnerRef, ticket: u64, cancel: bool)
                                   -> Result<Option<mac_device_interface::Allocation>, Status>
    {
        // Physical claim count is bounded by actual device inventory; no new
        // allocation-job registry or copied ownership token is introduced.
        let Some((physical, claim)) =
            self.claims.retained().find(|(_, claim)| {
                                      claim.owner.owner() == owner.owner()
                                      && claim.allocation.get().is_some_and(|(t, _)| t == ticket)
                                  })
        else
        {
            // Canonical broker cancellation may precede the first service turn.
            // No matching flight means no allocation effect from this request.
            return if cancel { Ok(None) } else { Err(NOT_FOUND) };
        };
        let (_, completion) = claim.allocation.get().ok_or(BAD_ABI)?;
        let id = completion.id();
        if cancel
        {
            let count = claim.maps.get().checked_sub(1).ok_or(BAD_ABI)?;
            // Unpublished storage was never exposed to DMA or a CPU client.
            if let Some(removed) = self.maps.remove(id)
            {
                claim.allocation.set(None);
                claim.maps.set(count);
                drop(removed);
            }
            else
            {
                // Invalidation may already belong to the canonical walk. The
                // original completion key proves retained payload identity;
                // cancelling publication does not free or refund that backing.
                let retained = self.maps.completing(&completion).ok_or(NOT_FOUND)?;
                if retained.buffer.as_ref().is_none_or(|buffer| buffer.ready())
                {
                    return Err(BAD_ABI);
                }
                claim.allocation.set(None);
            }
            return Ok(None);
        }
        if !owner.accepting()
        {
            return Err(mac_abi::OPERATION_CANCELLED);
        }
        let map = self.maps.get_mut(id).ok_or(NOT_FOUND)?;
        let buffer = Arc::get_mut(map.buffer.as_mut().ok_or(BAD_ABI)?).ok_or(SERVICE_BUSY)?;
        if !buffer.advance()
        {
            return Ok(None);
        }
        let uncached = buffer.uncached();
        let device_address =
            self.bus.device_address(physical.ordinal, buffer.device_address()?, buffer.length())?;
        let result = mac_device_interface::Allocation { handle: id,
                                                        cpu_address: buffer.address(),
                                                        device_address,
                                                        length: buffer.length() };
        claim.allocation.set(None);
        serial!("DEVICE_DMA_ALLOCATED owner={} allocation={} bytes={} cpu=0x{:x} bus=0x{:x} \
                 memory={}",
                owner.owner().incarnation(),
                id,
                result.length,
                result.cpu_address,
                result.device_address,
                if uncached { "uncached" } else { "cached" });
        Ok(Some(result))
    }

    #[cfg(systemrisc)]
    pub(crate) fn pin_buffer(&self, owner: &OwnerRef, id: u64)
                             -> Result<Arc<crate::machine::dma::Buffer>, Status>
    {
        let map = self.owned_map(owner, id)?;
        let (_, claim) = self.claims.completing(&map.claim).ok_or(BAD_ABI)?;
        if claim.cleanup.get() || claim.reset.get().is_some()
        {
            return Err(SERVICE_BUSY);
        }
        let buffer = map.buffer.as_ref().ok_or(BAD_ARGUMENT)?;
        if !buffer.ready()
        {
            return Err(SERVICE_BUSY);
        }
        Ok(buffer.clone())
    }

    #[cfg(systemrisc)]
    pub(crate) fn free_completed(&mut self, owner: &OwnerRef, id: u64) -> Result<(), Status>
    {
        let map = self.owned_map(owner, id)?;
        let (_, claim) = self.claims.completing(&map.claim).ok_or(BAD_ABI)?;
        let buffer = map.buffer.as_ref().ok_or(BAD_ARGUMENT)?;
        if !buffer.ready()
           || Arc::strong_count(buffer) != 1
           || claim.cleanup.get()
           || claim.reset.get().is_some()
        {
            return Err(SERVICE_BUSY);
        }
        let count = claim.maps.get().checked_sub(1).ok_or(BAD_ABI)?;
        let removed = self.maps.remove(id).ok_or(NOT_FOUND)?;
        claim.maps.set(count);
        drop(removed);
        serial!("DEVICE_DMA_FREED owner={} allocation={id}", owner.owner().incarnation());
        Ok(())
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

    /// Brings one mapping's two views into step. The span is checked against
    /// the mapping System recorded, never taken from the caller, so a driver
    /// cannot aim a cache operation at memory it did not map.
    pub(crate) fn sync(&mut self, owner: &OwnerRef, mapping: u64, offset: u64, length: u64,
                       for_device: bool)
                       -> Result<(), Status>
    {
        let map = self.owned_map(owner, mapping)?;
        let (physical, _) = self.claims.completing(&map.claim).ok_or(BAD_ABI)?;
        let coherent = physical.description.flags & mac_device_interface::COHERENT_DMA != 0;
        #[cfg(systemrisc)]
        let (span, cached) = match map.buffer.as_ref()
        {
            Some(buffer) => (Some((buffer.address(), buffer.length())), !buffer.uncached()),
            None => (map.span, true)
        };
        #[cfg(not(systemrisc))]
        let (span, cached) = (map.span, true);
        let (base, bytes) = span.ok_or(BAD_ARGUMENT)?;
        let end = offset.checked_add(length).ok_or(BAD_ARGUMENT)?;
        if length == 0 || end > bytes
        {
            return Err(BAD_ARGUMENT);
        }
        // Nothing to do where the device shares the caches, or where no cache
        // holds the storage. The call is still checked and accepted, so one
        // driver is correct on both kinds of machine.
        if coherent || !cached
        {
            return Ok(());
        }
        self.bus.synchronize(base + offset, length, for_device)
    }

    /// Publishes a view of firmware-owned memory as a mapping of this claim.
    pub(crate) fn shared(&mut self, owner: &OwnerRef, device: u64, address: u64, length: u64)
                         -> Result<mac_device_interface::View, Status>
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
        let cpu_address = self.bus.shared_view(physical.ordinal, address, length)?;
        let count = claim.maps.get().checked_add(1).ok_or(NO_MEMORY)?;
        let receipt = claim.receipt.get().ok_or(BAD_ABI)?;
        let publication = self.maps
                              .insert_with_receipt(owner, |_| {
                                  Dma { claim:                                  receipt,
                                        span:                                   Some((cpu_address,
                                                                                      length)),
                                        #[cfg(systemrisc)]
                                        buffer:                                 None }
                              })
                              .map_err(pool_status)?;
        claim.maps.set(count);
        Ok(mac_device_interface::View { handle: publication.id(),
                                        cpu_address,
                                        length })
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
        #[cfg(systemrisc)]
        if map.buffer
              .as_ref()
              .is_some_and(|buffer| !buffer.ready() || Arc::strong_count(buffer) != 1)
        {
            return Err(SERVICE_BUSY);
        }
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
                #[cfg(systemrisc)]
                let _execution = match claim.frame.exclude_execution()
                {
                    Ok(exclusion) => exclusion,
                    Err(_) =>
                    {
                        return Err(Failure::device_pending(ticket,
                                                           ordinal,
                                                           false,
                                                           claim.maps.get(),
                                                           claim.reset.get()));
                    }
                };
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
                #[cfg(systemrisc)]
                let _execution = match claim.frame.exclude_execution()
                {
                    Ok(exclusion) => exclusion,
                    Err(_) =>
                    {
                        return Err(Failure::device_pending(ticket,
                                                           ordinal,
                                                           false,
                                                           claim.maps.get(),
                                                           claim.reset.get()));
                    }
                };
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
                #[cfg(systemrisc)]
                if map.buffer.as_ref().is_some_and(|buffer| Arc::strong_count(buffer) != 1)
                {
                    return Err(Failure::device_pending(ticket,
                                                       ordinal,
                                                       true,
                                                       claim.maps.get(),
                                                       claim.reset.get()));
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
