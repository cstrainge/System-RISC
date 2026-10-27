//! Checked direct-map address facts. This view neither creates CPU mappings nor
//! changes the meaning of an existing reference. Native code must establish the
//! described mappings before lending mapped Rust views.
use core::fmt;

use mac_abi::{BootMemoryRange, boot::CheckedMemory};

use super::{PageLayout, PhysicalAddress, VirtualAddress};
use crate::policy::AddressRange;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    InvalidWindow,
    BankOutside
    {
        bank:          BootMemoryRange,
        physical_base: u64,
        bytes:         u64
    },
    UnmappedRam
    {
        physical: u64,
        bytes:    u64
    },
    OutsideWindow
    {
        virtual_address: u64,
        bytes:           u64
    },
    Overflow
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::InvalidWindow =>
            {
                f.write_str("direct-map window is unaligned, noncanonical, or incompatible with \
                             the selected page geometry")
            }
            Self::BankOutside { bank,
                                physical_base,
                                bytes } =>
            {
                write!(f,
                       "discovered RAM {:#x}+{:#x} lies outside direct-map physical window \
                        {physical_base:#x}+{bytes:#x}; select a larger platform map",
                       bank.base, bank.length)
            }
            Self::UnmappedRam { physical,
                                bytes } =>
            {
                write!(f,
                       "physical span {physical:#x}+{bytes:#x} is empty or not entirely backed by \
                        discovered RAM")
            }
            Self::OutsideWindow { virtual_address,
                                  bytes } =>
            {
                write!(f,
                       "CPU span {virtual_address:#x}+{bytes:#x} is empty or outside the \
                        direct-map window")
            }
            Self::Overflow => f.write_str("direct-map extent or address arithmetic overflow")
        }
    }
}
impl core::error::Error for Error
{
}

/// The layout policy supplies a physical origin explicitly. Physical topology
/// comes only from the validated boot account; holes are never made usable by
/// merely falling inside the virtual reservation.
#[derive(Clone, Copy)]
pub struct DirectMap<'a>
{
    memory:        CheckedMemory<'a>,
    virtual_range: AddressRange,
    physical_base: u64
}
impl<'a> DirectMap<'a>
{
    pub fn new<A: PageLayout>(memory: CheckedMemory<'a>, virtual_range: AddressRange,
                              physical_base: PhysicalAddress)
                              -> Result<Self, Error>
    {
        let geometry = A::geometry();
        if memory.page_bytes() != geometry.bytes()
           || !geometry.aligned(virtual_range.start())
           || !geometry.aligned(virtual_range.end())
           || !geometry.aligned(physical_base.0)
           || !A::valid_virtual_range(virtual_range)
        {
            return Err(Error::InvalidWindow);
        }
        let bytes = virtual_range.end() - virtual_range.start();
        let physical_end = physical_base.0.checked_add(bytes).ok_or(Error::Overflow)?;
        for bank in memory.banks()
        {
            // CheckedMemory already proved each bank's nonempty extent.
            if bank.base < physical_base.0 || bank.base + bank.length > physical_end
            {
                return Err(Error::BankOutside { bank: *bank,
                                                physical_base: physical_base.0,
                                                bytes });
            }
        }
        Ok(Self { memory,
                  virtual_range,
                  physical_base: physical_base.0 })
    }

    fn covers(&self, physical: u64, bytes: u64) -> Result<(), Error>
    {
        let end = physical.checked_add(bytes).ok_or(Error::Overflow)?;
        let mut cursor = physical;
        if bytes == 0
        {
            return Err(Error::UnmappedRam { physical,
                                            bytes });
        }
        while cursor < end
        {
            let Some(bank) =
                self.memory
                    .banks()
                    .iter()
                    .find(|bank| bank.base <= cursor && cursor < bank.base + bank.length)
            else
            {
                return Err(Error::UnmappedRam { physical,
                                                bytes });
            };
            // Adjacent banks can cover one contiguous physical span. A hole
            // still refuses; no ordering assumption or per-page walk is used.
            cursor = end.min(bank.base + bank.length);
        }
        Ok(())
    }

    pub fn virtual_address(&self, physical: PhysicalAddress, bytes: u64)
                           -> Result<VirtualAddress, Error>
    {
        self.covers(physical.0, bytes)?;
        Ok(VirtualAddress(self.virtual_range.start() + (physical.0 - self.physical_base)))
    }

    pub fn physical_address(&self, virtual_address: VirtualAddress, bytes: u64)
                            -> Result<PhysicalAddress, Error>
    {
        let end = virtual_address.0.checked_add(bytes).ok_or(Error::Overflow)?;
        if bytes == 0
           || virtual_address.0 < self.virtual_range.start()
           || end > self.virtual_range.end()
        {
            return Err(Error::OutsideWindow { virtual_address: virtual_address.0,
                                              bytes });
        }
        let physical = self.physical_base + (virtual_address.0 - self.virtual_range.start());
        self.covers(physical, bytes)?;
        Ok(PhysicalAddress(physical))
    }

    /// Exactly one mapping extent per real bank, not one per physical page.
    pub fn banks(&self) -> impl Iterator<Item = super::Mapping> + '_
    {
        self.memory.banks().iter().map(|bank| {
                                      super::Mapping { physical:        PhysicalAddress(bank.base),
                                                       virtual_address:
                                                           VirtualAddress(self.virtual_range
                                                                              .start()
                                                                          + (bank.base
                                                                             - self.physical_base)),
                                                       bytes:           bank.length }
                                  })
    }
}
