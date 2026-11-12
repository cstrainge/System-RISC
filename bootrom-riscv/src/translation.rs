//! Owned firmware translation preparation. This code only constructs tables;
//! it performs no CSR write, privilege transition, or kernel-reference rebasing.
//! The native entry boundary must retain this owner while hardware uses it.
use alloc::{boxed::Box, vec::Vec};
use core::fmt;

use mac_abi::{BootMemoryRange, boot::CheckedMemory};
use mac_kernel_model::{pages::{Access, PageOwnership, Permissions, PhysicalAddress,
                               VirtualAddress,
                               physical::{DirectMap, Error as DirectMapError}},
                       policy::AddressRange};
use mac_riscv_pages::{Entry, Sv39,
                      mapping::{MapError, Mapper, Tables}};

#[derive(Debug)]
pub enum FrameError
{
    Allocation,
    OutsideReservation
    {
        physical: u64
    },
    ForeignTable
    {
        physical: u64
    },
    Slot
    {
        index: usize
    }
}
impl fmt::Display for FrameError
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Allocation =>
            {
                f.write_str("firmware heap cannot allocate translation-table backing")
            }
            Self::OutsideReservation { physical } =>
            {
                write!(f,
                       "firmware table allocation {physical:#x}+4096 is not inside a bootstrap \
                        reservation; root was not published")
            }
            Self::ForeignTable { physical } =>
            {
                write!(f, "translation table {physical:#x} is not owned by this firmware root")
            }
            Self::Slot { index } =>
            {
                write!(f, "firmware translation slot {index} is outside the 512-entry table")
            }
        }
    }
}
#[derive(Debug)]
pub enum Error
{
    DirectMap(DirectMapError),
    Image(mac_formats::Error),
    Tables(MapError<FrameError>),
    UserRegion
    {
        virtual_address: u64,
        bytes:           u64
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::DirectMap(error) => error.fmt(f),
            Self::Image(error) => error.fmt(f),
            Self::Tables(error) => error.fmt(f),
            Self::UserRegion { virtual_address,
                               bytes } =>
            {
                write!(f,
                       "firmware root refuses user-accessible VA {virtual_address:#x}+{bytes:#x}; \
                        bootstrap regions must be supervisor-only")
            }
        }
    }
}
impl core::error::Error for Error
{
}

/// Derive the same System page records for the disk ROM and native ABI probe.
/// Each actual segment memory extent retains its permissions, including an
/// explicit BSS-only segment. Reservation padding and inter-segment holes do not
/// become mappings. Physical reservation ownership remains in BootInfo.
/// Re-exported so this bootstrap and the aarch64 one cannot derive the
/// mapping record differently.
pub use mac_bootloader::image_regions::system_regions;

#[repr(C, align(4096))]
struct Table([u64; 512]);
const _: () = assert!(size_of::<Table>() == 4096 && align_of::<Table>() == 4096);
struct Frames<'a>
{
    memory: CheckedMemory<'a>,
    tables: Vec<Box<Table>>
}
impl Tables for Frames<'_>
{
    type Error = FrameError;
    type Descriptor = mac_riscv_pages::Entry;
    type Layout = mac_riscv_pages::Sv39;

    fn allocate_zeroed(&mut self) -> Result<PhysicalAddress, FrameError>
    {
        self.tables.try_reserve(1).map_err(|_| FrameError::Allocation)?;
        let table = Box::try_new(Table([0; 512])).map_err(|_| FrameError::Allocation)?;
        let physical = (&*table as *const Table) as u64;
        // ROM allocation still uses identity CPU addresses. This check refuses
        // accidental use of a virtual allocator or unreserved backing. Later
        // kernel tables use their page leases and mapped views, not this owner.
        if !self.memory.reserved(BootMemoryRange { base:   physical,
                                                   length: 4096 })
        {
            return Err(FrameError::OutsideReservation { physical });
        }
        self.tables.push(table);
        Ok(PhysicalAddress(physical))
    }

    fn load(&self, table: PhysicalAddress, index: usize) -> Result<u64, FrameError>
    {
        let table = self.tables
                        .iter()
                        .find(|page| (&***page as *const Table) as u64 == table.0)
                        .ok_or(FrameError::ForeignTable { physical: table.0 })?;
        table.0.get(index).copied().ok_or(FrameError::Slot { index })
    }

    fn store(&mut self, table: PhysicalAddress, index: usize, entry: Entry)
             -> Result<(), FrameError>
    {
        let table = self.tables
                        .iter_mut()
                        .find(|page| (&***page as *const Table) as u64 == table.0)
                        .ok_or(FrameError::ForeignTable { physical: table.0 })?;
        *table.0.get_mut(index).ok_or(FrameError::Slot { index })? = entry.bits();
        Ok(())
    }
}
/// Code/data/MMIO extents are supplied by checked platform/image descriptions.
/// Root preparation copies no payload bytes and grants only these permissions.
pub struct Region
{
    pub virtual_address: VirtualAddress,
    pub physical:        PhysicalAddress,
    pub bytes:           u64,
    pub permissions:     Permissions
}
/// A firmware-owned temporary root, not a kernel AddressSpace. Its table boxes
/// stay pinned in the already-reserved firmware heap. The permanent kernel
/// root is subsequently built from PageBox leases before ordinary heap startup.
pub struct Prepared<'a>
{
    mapper: Mapper<Frames<'a>>,
    ram:    DirectMap<'a>,
    leaves: u64
}
impl<'a> Prepared<'a>
{
    pub fn new(memory: CheckedMemory<'a>, physmap: AddressRange, physical_origin: PhysicalAddress,
               regions: impl IntoIterator<Item = Region>)
               -> Result<Self, Error>
    {
        let ram =
            DirectMap::new::<Sv39>(memory, physmap, physical_origin).map_err(Error::DirectMap)?;
        let mut mapper = Mapper::new(Frames { memory,
                                              tables: Vec::new() }).map_err(Error::Tables)?;
        let mut leaves = 0;
        for bank in ram.banks()
        {
            let (mapped, _) = mapper.map_range(bank.virtual_address.0,
                                               bank.physical,
                                               bank.bytes,
                                               Permissions { uncached: false, device:  false,
                                                             read:    true,
                                                             write:   true,
                                                             execute: false,
                                                             access:  Access::Supervisor,
                                                             global:  true },
                                               PageOwnership::Manual)
                                    .map_err(Error::Tables)?;
            leaves += mapped;
        }
        for region in regions
        {
            if region.permissions.access != Access::Supervisor
            {
                return Err(Error::UserRegion { virtual_address: region.virtual_address.0,
                                               bytes:           region.bytes });
            }
            let (mapped, _) = mapper.map_range(region.virtual_address.0,
                                               region.physical,
                                               region.bytes,
                                               region.permissions,
                                               PageOwnership::Manual)
                                    .map_err(Error::Tables)?;
            leaves += mapped;
        }
        Ok(Self { mapper,
                  ram,
                  leaves })
    }

    pub fn root_physical(&self) -> PhysicalAddress
    {
        self.mapper.root()
    }

    pub fn cpu_address(&self, physical: PhysicalAddress, bytes: u64)
                       -> Result<VirtualAddress, DirectMapError>
    {
        self.ram.virtual_address(physical, bytes)
    }

    pub fn leaf_count(&self) -> u64
    {
        self.leaves
    }
}

