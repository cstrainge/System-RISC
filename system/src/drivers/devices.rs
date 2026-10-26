//! Resident device/DMA lifetime policy. Bus-specific register access lives in a
//! machine adapter. Every claim/map ID comes from the shared object facility.
#![forbid(unsafe_code)]
use mac_abi::{ACCESS_DENIED, BAD_ARGUMENT, NO_MEMORY, NOT_FOUND, SERVICE_BUSY, Status};
use mac_device_interface::{Description, Mapping};

use crate::toolbox::handles::{Kind, Table};
pub trait Bus
{
    fn probe(&mut self, ordinal: u32) -> Result<Description, Status>;
    fn request_reset(&mut self, ordinal: u32) -> Result<(), Status>;
    fn stopped(&mut self, ordinal: u32) -> Result<bool, Status>;
}
struct Claim
{
    owner:       u64,
    ordinal:     u32,
    description: Description,
    reset:       Option<u64>
}
struct Dma
{
    owner:   u64,
    device:  u32,
    mapping: Mapping
}
pub struct Devices<B>
{
    bus:    B,
    claims: Table<Claim>,
    maps:   Table<Dma>
}
impl<B: Bus> Devices<B>
{
    pub const fn new(bus: B) -> Self
    {
        Self { bus,
               claims: Table::new(Kind::DriverDevices),
               maps: Table::new(Kind::DriverDma) }
    }

    pub fn probe(&mut self, ordinal: u32) -> Result<Description, Status>
    {
        self.bus.probe(ordinal)
    }

    fn owned(&self, owner: u64, device: u32) -> Result<&Claim, Status>
    {
        let claim = self.claims.get(device).ok_or(NOT_FOUND)?;
        if owner == 0 || claim.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        Ok(claim)
    }

    /// Authorization has checked this exact bus/match key against the admitted
    /// contract. Re-probe before publication so changed hardware is not claimed.
    pub fn claim(&mut self, owner: u64, ordinal: u32, expected: Description)
                 -> Result<Description, Status>
    {
        if owner == 0 || expected.flags & mac_device_interface::RESIDENT_OWNED != 0
        {
            return Err(ACCESS_DENIED);
        }
        if self.claims.iter().any(|(_, c)| c.ordinal == ordinal)
        {
            return Err(SERVICE_BUSY);
        }
        if self.bus.probe(ordinal)? != expected
        {
            return Err(NOT_FOUND);
        }
        let id = self.claims
                     .insert(Claim { owner,
                                     ordinal,
                                     description: expected,
                                     reset: None })
                     .map_err(|_| NO_MEMORY)?;
        Ok(Description { handle: id,
                         ..expected })
    }

    /// The caller checked the whole RAM range against its pinned module image.
    /// This board uses coherent identity DMA; another bus supplies its mapping
    /// adapter without changing the module entry or driver request ABI.
    pub fn map(&mut self, owner: u64, device: u32, address: u64, length: u64)
               -> Result<Mapping, Status>
    {
        let claim = self.owned(owner, device)?;
        if claim.reset.is_some()
        {
            return Err(SERVICE_BUSY);
        }
        if length == 0 || address.checked_add(length).is_none()
        {
            return Err(BAD_ARGUMENT);
        }
        if claim.description.flags & mac_device_interface::COHERENT_DMA == 0
        {
            return Err(mac_abi::NOT_SUPPORTED);
        }
        let id = self.maps
                     .insert_with(|id| {
                         Dma { owner,
                               device,
                               mapping: Mapping { handle: id,
                                                  address,
                                                  length } }
                     })
                     .map_err(|_| NO_MEMORY)?;
        Ok(self.maps.get(id).ok_or(NOT_FOUND)?.mapping)
    }

    pub fn reset(&mut self, owner: u64, device: u32, ticket: u64, start: bool)
                 -> Result<bool, Status>
    {
        if ticket == 0
        {
            return Err(BAD_ARGUMENT);
        }
        let claim = self.owned(owner, device)?;
        let ordinal = claim.ordinal;
        if start
        {
            if claim.reset.is_some()
            {
                return Err(SERVICE_BUSY);
            }
            // A bus error still leaves unmap/release gated on a positive
            // stopped acknowledgement; no failed write frees live DMA.
            self.claims.get_mut(device).ok_or(NOT_FOUND)?.reset = Some(ticket);
            if let Err(error) = self.bus.request_reset(ordinal)
            {
                self.claims.get_mut(device).ok_or(NOT_FOUND)?.reset = None;
                return Err(error);
            }
        }
        else if claim.reset != Some(ticket)
        {
            return Err(ACCESS_DENIED);
        }
        match self.bus.stopped(ordinal)
        {
            Ok(true) =>
            {
                self.claims.get_mut(device).ok_or(NOT_FOUND)?.reset = None;
                Ok(true)
            }
            Ok(false) => Ok(false),
            Err(error) =>
            {
                self.claims.get_mut(device).ok_or(NOT_FOUND)?.reset = None;
                Err(error)
            }
        }
    }

    pub fn reset_pending(&mut self, owner: u64, ticket: u64) -> Result<bool, Status>
    {
        let device = self.claims
                         .iter()
                         .find(|(_, c)| c.owner == owner && c.reset == Some(ticket))
                         .map(|(id, _)| id)
                         .ok_or(NOT_FOUND)?;
        self.reset(owner, device, ticket, false)
    }

    pub fn unmap(&mut self, owner: u64, mapping: u32) -> Result<(), Status>
    {
        let dma = self.maps.get(mapping).ok_or(NOT_FOUND)?;
        if dma.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        let claim = self.owned(owner, dma.device)?;
        if claim.reset.is_some()
        {
            return Err(SERVICE_BUSY);
        }
        let ordinal = claim.ordinal;
        if !self.bus.stopped(ordinal)?
        {
            return Err(SERVICE_BUSY);
        }
        self.maps.remove(mapping);
        Ok(())
    }

    pub fn release(&mut self, owner: u64, device: u32) -> Result<(), Status>
    {
        let claim = self.owned(owner, device)?;
        if claim.reset.is_some() || self.maps.iter().any(|(_, m)| m.device == device)
        {
            return Err(SERVICE_BUSY);
        }
        let ordinal = claim.ordinal;
        if !self.bus.stopped(ordinal)?
        {
            return Err(SERVICE_BUSY);
        }
        self.claims.remove(device);
        Ok(())
    }
}
#[cfg(test)]
mod tests
{
    use super::*;
    struct TestBus
    {
        stopped: bool,
        resets:  u32
    }
    const DEVICE: Description = Description { handle:         0,
                                              bus:            mac_module::ClassId(*b"TEST"),
                                              key:            [3; 16],
                                              registers:      0x1000,
                                              register_bytes: 4096,
                                              interrupt:      4,
                                              flags:          mac_device_interface::COHERENT_DMA
                                                              | mac_device_interface::MMIO };
    impl Bus for TestBus
    {
        fn probe(&mut self, ordinal: u32) -> Result<Description, Status>
        {
            if ordinal == 0 { Ok(DEVICE) } else { Err(NOT_FOUND) }
        }

        fn request_reset(&mut self, _: u32) -> Result<(), Status>
        {
            self.resets += 1;
            Ok(())
        }

        fn stopped(&mut self, _: u32) -> Result<bool, Status>
        {
            Ok(self.stopped)
        }
    }
    #[test]
    fn claims_and_dma_keep_owners_and_reject_stale_foreign_or_reused_ids()
    {
        let mut manager = Devices::new(TestBus { stopped: true,
                                                 resets:  0 });
        let first = manager.claim(1, 0, DEVICE).unwrap();
        assert_eq!(manager.claim(2, 0, DEVICE), Err(SERVICE_BUSY));
        assert_eq!(manager.release(2, first.handle), Err(ACCESS_DENIED));
        let mapping = manager.map(1, first.handle, 0x8000_0000, 4096).unwrap();
        assert_eq!(manager.release(1, first.handle), Err(SERVICE_BUSY));
        assert_eq!(manager.unmap(2, mapping.handle), Err(ACCESS_DENIED));
        assert_eq!(manager.unmap(1, first.handle), Err(NOT_FOUND));
        manager.unmap(1, mapping.handle).unwrap();
        let next = manager.map(1, first.handle, 0x8000_0000, 4096).unwrap();
        assert_eq!(next.handle as u16, mapping.handle as u16);
        assert_ne!(next.handle, mapping.handle);
        assert_eq!(manager.unmap(1, mapping.handle), Err(NOT_FOUND));
        manager.unmap(1, next.handle).unwrap();
        manager.release(1, first.handle).unwrap();
        let second = manager.claim(1, 0, DEVICE).unwrap();
        assert_eq!(first.handle as u16, second.handle as u16);
        assert_ne!(first.handle, second.handle);
        assert_eq!(manager.map(1, first.handle, 0x8000_0000, 4096), Err(NOT_FOUND));
        let mut foreign = Devices::new(TestBus { stopped: true,
                                                 resets:  0 });
        assert_ne!(foreign.claim(1, 0, DEVICE).unwrap().handle, second.handle);
        assert_eq!(foreign.release(1, second.handle), Err(NOT_FOUND));
    }
    #[test]
    fn reset_does_bounded_progress_and_never_releases_live_dma()
    {
        let mut manager = Devices::new(TestBus { stopped: false,
                                                 resets:  0 });
        let device = manager.claim(1, 0, DEVICE).unwrap();
        let map = manager.map(1, device.handle, 0x8000, 128).unwrap();
        assert_eq!(manager.unmap(1, map.handle), Err(SERVICE_BUSY));
        assert_eq!(manager.reset(1, device.handle, 0x10001, true), Ok(false));
        assert_eq!(manager.reset(1, device.handle, 0x10002, false), Err(ACCESS_DENIED));
        assert_eq!(manager.reset(1, device.handle, 0x10001, false), Ok(false));
        assert_eq!(manager.bus.resets, 1);
        assert_eq!(manager.release(1, device.handle), Err(SERVICE_BUSY));
        manager.bus.stopped = true;
        assert_eq!(manager.unmap(1, map.handle), Err(SERVICE_BUSY));
        assert_eq!(manager.reset(1, device.handle, 0x10001, false), Ok(true));
        manager.unmap(1, map.handle).unwrap();
        manager.release(1, device.handle).unwrap();
    }
}
