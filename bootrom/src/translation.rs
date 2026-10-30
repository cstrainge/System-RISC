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
pub fn system_regions(image: &mac_formats::native::image::Image,
                      descriptor: &mac_formats::native::Descriptor, physical_base: u64, page: u64)
                      -> Result<Vec<mac_abi::boot::execution::Region>, Error>
{
    use mac_abi::{BootMemoryRange,
                  boot::execution::{EXECUTE, READ, Region, WRITE}};
    use mac_formats::Error as FormatError;
    let invalid = || {
        Error::Image(FormatError::Malformed("System mapping segments must be page aligned, \
                                             nonoverlapping and inside the native reservation"))
    };
    if descriptor.role != mac_formats::native::Role::System
       || !page.is_power_of_two()
       || page < 4096
       || !physical_base.is_multiple_of(page)
    {
        return Err(invalid());
    }
    let extent = descriptor.load_limit.checked_sub(descriptor.load_base).ok_or_else(invalid)?;
    if extent == 0
       || !extent.is_multiple_of(page)
       || !descriptor.load_base.is_multiple_of(page)
       || physical_base.checked_add(extent).is_none()
    {
        return Err(invalid());
    }
    let mut regions = Vec::new();
    regions.try_reserve_exact(image.segments.len())
        .map_err(|_|Error::Image(FormatError::AllocationFailed("System mapping record allocation failed")))?;
    let mut last = 0;
    for segment in &image.segments
    {
        let end = segment.offset
                         .checked_add(segment.memory_length)
                         .and_then(|end| end.checked_add(page - 1))
                         .map(|end| end / page * page)
                         .ok_or_else(invalid)?;
        if segment.memory_length == 0
           || segment.length as u64 > segment.memory_length
           || segment.flags & !7 != 0
           || segment.flags & 3 == 3
           || !segment.offset.is_multiple_of(page)
           || segment.offset < last
           || end > extent
           || end <= segment.offset
        {
            return Err(invalid());
        }
        let flags = (if segment.flags & 4 != 0 { READ } else { 0 })
                    | (if segment.flags & 2 != 0 { WRITE } else { 0 })
                    | (if segment.flags & 1 != 0 { EXECUTE } else { 0 });
        regions.push(Region { physical:
                                  BootMemoryRange { base:
                                                        physical_base.checked_add(segment.offset)
                                                                     .ok_or_else(invalid)?,
                                                    length: end - segment.offset },
                              virtual_base: descriptor.load_base
                                                      .checked_add(segment.offset)
                                                      .ok_or_else(invalid)?,
                              flags,
                              reserved: 0 });
        last = end;
    }
    Ok(regions)
}

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
                                               Permissions { read:    true,
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

#[cfg(test)]
mod tests
{
    extern crate std;
    use std::string::ToString;

    use super::*;

    fn system_image() -> (mac_formats::native::Descriptor, mac_formats::native::image::Image)
    {
        use mac_formats::native::{Descriptor, Role,
                                  image::{Image, Segment}};
        (Descriptor { role:        Role::System,
                      abi_version: 1,
                      load_base:   0xffffffd000200000,
                      load_limit:  0xffffffd000205000,
                      entry:       0xffffffd000200000 },
         Image { segments: alloc::vec![Segment { offset:        0,
                                                 length:        32,
                                                 memory_length: 32,
                                                 flags:         5,
                                                 data_offset:   0 },
                                       Segment { offset:        4096,
                                                 length:        19,
                                                 memory_length: 19,
                                                 flags:         4,
                                                 data_offset:   32 },
                                       Segment { offset:        8192,
                                                 length:        11,
                                                 memory_length: 11,
                                                 flags:         6,
                                                 data_offset:   51 },
                                       Segment { offset:        12288,
                                                 length:        0,
                                                 memory_length: 8192,
                                                 flags:         6,
                                                 data_offset:   62 }] })
    }
    #[test]
    fn system_pages_preserve_explicit_segment_memory_permissions()
    {
        use mac_abi::boot::execution::{EXECUTE, READ, WRITE};
        let (descriptor, image) = system_image();
        let regions = system_regions(&image, &descriptor, 0x80200000, 4096).unwrap();
        assert_eq!(regions.len(), 4);
        for (index, region) in regions.iter().enumerate()
        {
            assert_eq!(region.physical.base, 0x80200000 + (index as u64) * 4096);
            assert_eq!(region.virtual_base, descriptor.load_base + (index as u64) * 4096);
            assert_eq!(region.reserved, 0);
        }
        assert_eq!(regions[0].flags, READ | EXECUTE);
        assert_eq!(regions[1].flags, READ);
        assert_eq!(regions[2].flags, READ | WRITE);
        assert_eq!(regions[3].flags, READ | WRITE);
        assert_eq!(regions[3].physical.length, 8192);
    }
    #[test]
    fn misaligned_overlapping_and_out_of_extent_system_pages_refuse_with_detail()
    {
        for case in 0..6
        {
            let (mut descriptor, mut image) = system_image();
            let mut physical = 0x80200000;
            match case
            {
                0 => image.segments[1].offset = 4095,
                1 => image.segments[1].offset = 0,
                2 => image.segments[2].length = 0x10000,
                3 => descriptor.load_limit -= 1,
                4 => physical += 1,
                _ => descriptor.role = mac_formats::native::Role::Application
            }
            let error = system_regions(&image, &descriptor, physical, 4096).unwrap_err();
            assert!(matches!(error, Error::Image(mac_formats::Error::Malformed(_))));
            assert!(error.to_string().contains("page aligned, nonoverlapping and inside"));
        }
    }

    #[test]
    fn frames_validate_reservation_ownership_and_slots_before_publication()
    {
        // Pure host backing tests. The mock address account permits every
        // possible host heap pointer below the architecture's PA ceiling;
        // actual CPU activation/boot is qualified separately in native guests.
        let ram = [BootMemoryRange { base:   0,
                                     length: 1 << 56 }];
        let memory = mac_abi::boot::Memory { ram:          &ram,
                                             reservations: &ram }.checked(4096, ram[0])
                                                                 .unwrap();
        let mut frames = Frames { memory,
                                  tables: Vec::new() };
        let root = frames.allocate_zeroed().unwrap();
        assert!(root.0.is_multiple_of(4096));
        assert_eq!(frames.tables.len(), 1);
        assert_eq!(frames.load(root, 0).unwrap(), 0);
        let entry =
            Entry::table(PhysicalAddress(0x90000000), false, PageOwnership::Manual).unwrap();
        frames.store(root, 0, entry).unwrap();
        assert_eq!(frames.load(root, 0).unwrap(), entry.bits());
        assert!(matches!(frames.store(root, 512, entry), Err(FrameError::Slot { index: 512 })));
        assert!(matches!(frames.load(PhysicalAddress(0xdead000), 0),
                         Err(FrameError::ForeignTable { .. })));
        assert_eq!(frames.load(root, 0).unwrap(), entry.bits(), "refusal changed a published slot");
        assert!(FrameError::ForeignTable { physical: 0xdead000 }.to_string().contains("not owned"));
        assert!(FrameError::Slot { index: 512 }.to_string().contains("512-entry"));
        let memory = mac_abi::boot::Memory { ram:          &ram,
                                             reservations: &[] }.checked(4096, ram[0])
                                                                .unwrap();
        let mut frames = Frames { memory,
                                  tables: Vec::new() };
        let error = frames.allocate_zeroed().unwrap_err();
        assert!(matches!(error, FrameError::OutsideReservation { .. }));
        assert!(error.to_string().contains("not inside a bootstrap reservation"));
        assert!(frames.tables.is_empty(), "unreserved frame was published");
    }
}
