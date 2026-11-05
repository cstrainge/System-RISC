//! RV64 Sv39 encoding, separate from physical ownership and CPU instructions.
//! Architecture reference: RISC-V Privileged ISA, Supervisor chapter, Sv39.
//! No pointer dereference, satp write or implicit activation exists in this crate.
#![no_std]
#![forbid(unsafe_code)]
pub mod mapping;
pub mod walk;
use core::fmt;

use mac_kernel_model::{pages::{Access, PageOwnership, Permissions, PhysicalAddress},
                       policy::AddressRange};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sv39;
impl mac_kernel_model::pages::PageLayout for Sv39
{
    fn geometry() -> mac_kernel_model::pages::PageGeometry
    {
        mac_kernel_model::pages::PageGeometry::new(Self::PAGE_BYTES).expect("Sv39 base pages are \
                                                                             a power-of-two 4096 \
                                                                             bytes")
    }

    fn leaf_sizes() -> &'static [u64]
    {
        Self::LEAF_BYTES
    }

    fn valid_virtual_range(range: AddressRange) -> bool
    {
        Self::canonical_range(range)
    }

    fn lower_half_end() -> u64
    {
        1 << 38
    }

    fn upper_half_start() -> u64
    {
        u64::MAX << 38
    }
}
/// The supervisor root register value: the Sv39 mode field, the address-space
/// tag and the root table's physical page number, encoded together. Callers
/// never re-derive the mode field or the PPN mask, so the encoding has exactly
/// one definition and a second architecture states its own register form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Root(u64);
impl Root
{
    pub const ASID_BITS: u32 = 16;
    pub const ASID_MASK: u64 = (1 << Self::ASID_BITS) - 1;
    pub const ASID_SHIFT: u32 = 44;
    /// Sv39 is mode 8 in the satp mode field.
    pub const MODE: u64 = 8;
    pub const MODE_SHIFT: u32 = 60;
    pub const PPN_MASK: u64 = (1 << 44) - 1;
    pub const SHIFT: u32 = 12;

    /// Encode a root table address and address-space tag. The address must be
    /// page aligned and the tag must fit the ASID field; both are refused rather
    /// than truncated.
    pub fn from_physical(physical: PhysicalAddress, tag: u64) -> Result<Self, Error>
    {
        if physical.0 & (Sv39::PAGE_BYTES - 1) != 0
        {
            return Err(Error::Physical { address:   physical.0,
                                         alignment: Sv39::PAGE_BYTES });
        }
        if tag > Self::ASID_MASK
        {
            return Err(Error::AddressSpaceTag { tag });
        }
        let ppn = physical.0 >> Self::SHIFT;
        if ppn > Self::PPN_MASK
        {
            return Err(Error::Physical { address:   physical.0,
                                         alignment: Sv39::PAGE_BYTES });
        }
        Ok(Self((Self::MODE << Self::MODE_SHIFT) | (tag << Self::ASID_SHIFT) | ppn))
    }

    /// Admit an observed register value, refusing any mode this crate does not
    /// encode. A warm root comparison must not accept a differently-moded
    /// register that happens to share the low bits.
    pub fn decode(bits: u64) -> Result<Self, Error>
    {
        let mode = bits >> Self::MODE_SHIFT;
        if mode != Self::MODE
        {
            return Err(Error::RootMode { mode });
        }
        Ok(Self(bits))
    }

    pub const fn bits(self) -> u64
    {
        self.0
    }

    pub fn physical(self) -> PhysicalAddress
    {
        PhysicalAddress((self.0 & Self::PPN_MASK) << Self::SHIFT)
    }

    pub fn tag(self) -> u64
    {
        (self.0 >> Self::ASID_SHIFT) & Self::ASID_MASK
    }

    /// Whether this register names the given root table, ignoring the tag.
    pub fn names(self, physical: PhysicalAddress) -> bool
    {
        self.physical() == physical
    }
}
impl Sv39
{
    pub const ADDRESS_BITS: u32 = 39;
    pub const ENTRIES: usize = 512;
    pub const LEAF_BYTES: &'static [u64] = &[1 << 30, 1 << 21, 1 << 12];
    pub const PAGE_BYTES: u64 = 4096;

    pub const fn canonical(address: u64) -> bool
    {
        ((address << 25) as i64 >> 25) as u64 == address
    }

    pub fn canonical_range(range: AddressRange) -> bool
    {
        Self::canonical(range.start())
        && Self::canonical(range.end() - 1)
        && range.start() >> 38 == (range.end() - 1) >> 38
    }

    pub fn index(address: u64, level: u8) -> Result<usize, Error>
    {
        let size = LeafSize::from_level(level)?;
        if !Self::canonical(address)
        {
            return Err(Error::NonCanonical { address });
        }
        // The arithmetic itself is geometry and lives on `PageLayout`, so this
        // format and any other 39-bit three-level format compute an index the
        // same way. This function only translates a level number into a leaf
        // size, which is the part that differs between formats.
        <Self as mac_kernel_model::pages::PageLayout>::index(address, size.bytes())
            .ok_or(Error::Level { level })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeafSize
{
    Page,
    Large,
    Huge
}
impl LeafSize
{
    pub const fn bytes(self) -> u64
    {
        1 << self.shift()
    }

    pub const fn level(self) -> u8
    {
        match self
        {
            Self::Page => 0,
            Self::Large => 1,
            Self::Huge => 2
        }
    }

    const fn shift(self) -> u32
    {
        12 + 9 * (self.level() as u32)
    }

    pub fn from_level(level: u8) -> Result<Self, Error>
    {
        match level
        {
            0 => Ok(Self::Page),
            1 => Ok(Self::Large),
            2 => Ok(Self::Huge),
            _ => Err(Error::Level { level })
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    NonCanonical
    {
        address: u64
    },
    VirtualAlignment
    {
        address:   u64,
        alignment: u64
    },
    Level
    {
        level: u8
    },
    Physical
    {
        address:   u64,
        alignment: u64
    },
    Permissions,
    ReservedBits
    {
        bits: u64
    },
    Ownership
    {
        bits: u8
    },
    NonLeafAtBottom,
    /// An address-space tag does not fit the ASID field, or a decoded root
    /// register does not name the Sv39 mode.
    AddressSpaceTag
    {
        tag: u64
    },
    RootMode
    {
        mode: u64
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::NonCanonical { address } =>
            {
                write!(f, "Sv39 address {address:#x} is outside the canonical virtual ranges")
            }
            Self::VirtualAlignment { address,
                                     alignment } =>
            {
                write!(f, "Sv39 virtual address {address:#x} must be aligned to {alignment} bytes")
            }
            Self::Level { level } =>
            {
                write!(f, "Sv39 page-table level {level} is invalid; expected 0 through 2")
            }
            Self::Physical { address,
                             alignment } =>
            {
                write!(f,
                       "Sv39 physical address {address:#x} must fit 56 bits and be aligned to \
                        {alignment} bytes")
            }
            Self::Permissions =>
            {
                f.write_str("Sv39 valid leaf requires read or execute, write requires read, and \
                             copy-on-write must be read-only")
            }
            Self::ReservedBits { bits } =>
            {
                write!(f, "Sv39 PTE has reserved or unsupported attribute bits {bits:#x}")
            }
            Self::Ownership { bits } =>
            {
                write!(f,
                       "Sv39 software ownership bits {bits} have no defined frame-ownership \
                        contract")
            }
            Self::NonLeafAtBottom =>
            {
                f.write_str("Sv39 level 0 entry cannot refer to a lower page table")
            }
            Self::AddressSpaceTag { tag } =>
            {
                write!(f, "address-space tag {tag:#x} does not fit the 16-bit ASID field")
            }
            Self::RootMode { mode } =>
            {
                write!(f, "root register mode {mode} is not Sv39; this crate encodes Sv39 only")
            }
        }
    }
}
impl core::error::Error for Error
{
}
const VALID: u64 = 1;
const READ: u64 = 1 << 1;
const WRITE: u64 = 1 << 2;
const EXECUTE: u64 = 1 << 3;
const USER: u64 = 1 << 4;
const GLOBAL: u64 = 1 << 5;
const ACCESSED: u64 = 1 << 6;
const DIRTY: u64 = 1 << 7;
const PPN_MASK: u64 = ((1u64 << 44) - 1) << 10;
const SOFTWARE_MASK: u64 = 3 << 8;
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Entry(u64);
pub enum Decoded
{
    Invalid,
    Table
    {
        physical:  PhysicalAddress,
        global:    bool,
        ownership: PageOwnership
    },
    Leaf
    {
        physical:    PhysicalAddress,
        size:        LeafSize,
        permissions: Permissions,
        ownership:   PageOwnership
    }
}
fn ownership_bits(ownership: PageOwnership) -> u64
{
    match ownership
    {
        PageOwnership::Manual => 0,
        PageOwnership::Automatic => 1 << 8,
        PageOwnership::CopyOnWrite => 2 << 8
    }
}
fn physical_bits(address: PhysicalAddress, alignment: u64) -> Result<u64, Error>
{
    if address.0 >= 1 << 56 || !address.0.is_multiple_of(alignment)
    {
        return Err(Error::Physical { address: address.0,
                                     alignment });
    }
    Ok((address.0 >> 12) << 10)
}
impl Entry
{
    pub const fn invalid() -> Self
    {
        Self(0)
    }

    pub const fn bits(self) -> u64
    {
        self.0
    }

    pub fn table(address: PhysicalAddress, global: bool, ownership: PageOwnership)
                 -> Result<Self, Error>
    {
        Ok(Self(physical_bits(address, Sv39::PAGE_BYTES)?
                | VALID
                | if global { GLOBAL } else { 0 }
                | ownership_bits(ownership)))
    }

    pub fn leaf(address: PhysicalAddress, size: LeafSize, permissions: Permissions,
                ownership: PageOwnership)
                -> Result<Self, Error>
    {
        if (!permissions.read && !permissions.execute)
           || (permissions.write && !permissions.read)
           || (ownership == PageOwnership::CopyOnWrite && permissions.write)
        {
            return Err(Error::Permissions);
        }
        let mut flags = VALID | ACCESSED | ownership_bits(ownership);
        for (enabled, flag) in [(permissions.read, READ),
                                (permissions.write, WRITE),
                                (permissions.execute, EXECUTE),
                                (matches!(permissions.access, Access::User), USER),
                                (permissions.global, GLOBAL),
                                (permissions.write, DIRTY)]
        {
            if enabled
            {
                flags |= flag;
            }
        }
        Ok(Self(physical_bits(address, size.bytes())? | flags))
    }

    pub fn decode(self, level: u8) -> Result<Decoded, Error>
    {
        Self::decode_raw(self.bits(), level)
    }

    /// Decode hardware memory without turning an unchecked word into an entry
    /// suitable for installation. Writers accept only validated constructors.
    pub fn decode_raw(bits: u64, level: u8) -> Result<Decoded, Error>
    {
        let size = LeafSize::from_level(level)?;
        if bits & VALID == 0
        {
            return Ok(Decoded::Invalid);
        }
        let reserved = bits & !(PPN_MASK | SOFTWARE_MASK | 255);
        if reserved != 0
        {
            return Err(Error::ReservedBits { bits: reserved });
        }
        let ownership = match (bits >> 8) & 3
        {
            0 => PageOwnership::Manual,
            1 => PageOwnership::Automatic,
            2 => PageOwnership::CopyOnWrite,
            _ => return Err(Error::Ownership { bits: 3 })
        };
        let physical = PhysicalAddress((bits & PPN_MASK) << 2);
        if bits & (READ | WRITE | EXECUTE) == 0
        {
            if level == 0
            {
                return Err(Error::NonLeafAtBottom);
            }
            let reserved = bits & (USER | ACCESSED | DIRTY);
            if reserved != 0
            {
                return Err(Error::ReservedBits { bits: reserved });
            }
            return Ok(Decoded::Table { physical,
                                       global: bits & GLOBAL != 0,
                                       ownership });
        }
        let permissions =
            Permissions { read:    bits & READ != 0,
                          write:   bits & WRITE != 0,
                          execute: bits & EXECUTE != 0,
                          access:  if bits & USER != 0 { Access::User } else { Access::Supervisor },
                          global:  bits & GLOBAL != 0 };
        Self::leaf(physical, size, permissions, ownership)?;
        Ok(Decoded::Leaf { physical,
                           size,
                           permissions,
                           ownership })
    }
}
#[cfg(test)]
mod tests
{
    use super::*;
    extern crate alloc;
    use alloc::string::ToString;
    fn permissions() -> Permissions
    {
        Permissions { read:    true,
                      write:   false,
                      execute: true,
                      access:  Access::Supervisor,
                      global:  true }
    }
    #[test]
    fn index_is_geometry_and_agrees_for_every_leaf_size()
    {
        // The arithmetic moved onto `PageLayout`; this pins the values it must
        // still produce for the addresses the kernel actually maps, so a later
        // architecture cannot quietly change what Sv39 computes.
        for (address, page, large, huge) in [
            (0x0u64, 0, 0, 0),
            (0x1000, 1, 0, 0),
            (0x200000, 0, 1, 0),
            (0x40000000, 0, 0, 1),
            (0x80200000, 0, 1, 2),
            (0xffffffc000000000, 0, 0, 256),
        ]
        {
            assert_eq!(Sv39::index(address, 0).unwrap(), page, "page {address:#x}");
            assert_eq!(Sv39::index(address, 1).unwrap(), large, "large {address:#x}");
            assert_eq!(Sv39::index(address, 2).unwrap(), huge, "huge {address:#x}");
        }
        // A level outside the format, or a leaf the format does not declare, is
        // refused rather than producing a meaningless index.
        assert!(Sv39::index(0, 3).is_err());
        assert!(<Sv39 as mac_kernel_model::pages::PageLayout>::index(0, 8192).is_none());
    }

    #[test]
    fn canonical_ranges_reject_the_hole_and_crossing_with_canonical_endpoints()
    {
        for value in [0, 0x3fffffffff, 0xffffffc000000000, u64::MAX]
        {
            assert!(Sv39::canonical(value));
        }
        for value in [0x4000000000, 0xffffffbfffffffff]
        {
            assert!(!Sv39::canonical(value));
        }
        assert!(!Sv39::canonical_range(AddressRange::new(4096, 0xffffffc000001000).unwrap()));
        assert!(Sv39::canonical_range(AddressRange::new(0xffffffc000000000,0xfffffffffffff000).unwrap()));
        assert_eq!(Sv39::index(0xffffffe000000000, 2).unwrap(), 384);
        assert!(Sv39::index(0x4000000000, 2).unwrap_err().to_string().contains("canonical"));
        assert!(Sv39::index(0, 3).unwrap_err().to_string().contains("level 3"));
    }
    #[test]
    fn leaves_round_trip_all_sizes_and_keep_physical_ownership_and_permissions()
    {
        for size in [LeafSize::Page, LeafSize::Large, LeafSize::Huge]
        {
            for ownership in
                [PageOwnership::Manual, PageOwnership::Automatic, PageOwnership::CopyOnWrite]
            {
                let entry = Entry::leaf(PhysicalAddress(0x80000000),
                                        size,
                                        permissions(),
                                        ownership).unwrap();
                assert_ne!(entry.bits() & ACCESSED, 0);
                assert_eq!(entry.bits() & DIRTY, 0);
                let Decoded::Leaf { physical,
                                    size: actual,
                                    permissions: access,
                                    ownership: kind } = entry.decode(size.level()).unwrap()
                else
                {
                    panic!("leaf lost its kind")
                };
                assert_eq!(physical, PhysicalAddress(0x80000000));
                assert_eq!(actual, size);
                assert_eq!(kind, ownership);
                assert!(access.read && access.execute && access.global && !access.write);
                assert_eq!(access.access, Access::Supervisor);
            }
        }
        let mut access = permissions();
        access.write = true;
        access.execute = false;
        access.access = Access::User;
        let entry = Entry::leaf(PhysicalAddress(4096),
                                LeafSize::Page,
                                access,
                                PageOwnership::Automatic).unwrap();
        assert_eq!(entry.bits() & (USER | DIRTY | ACCESSED), USER | DIRTY | ACCESSED);
    }
    #[test]
    fn malformed_physical_addresses_flags_and_reserved_attributes_fail_typed()
    {
        for (address, size) in
            [(4096, LeafSize::Huge), (1 << 56, LeafSize::Page), (1, LeafSize::Page)]
        {
            assert!(matches!(Entry::leaf(PhysicalAddress(address),
                                         size,
                                         permissions(),
                                         PageOwnership::Manual),
                             Err(Error::Physical { .. })));
        }
        let mut p = permissions();
        p.read = false;
        p.write = true;
        assert_eq!(Entry::leaf(PhysicalAddress(0), LeafSize::Page, p, PageOwnership::Manual),
                   Err(Error::Permissions));
        let mut p = permissions();
        p.write = true;
        assert_eq!(Entry::leaf(PhysicalAddress(0), LeafSize::Page, p, PageOwnership::CopyOnWrite),
                   Err(Error::Permissions));
        assert!(matches!(Entry::decode_raw(VALID | READ | 1 << 63, 0),
                         Err(Error::ReservedBits { .. })));
        assert!(matches!(Entry::decode_raw(VALID | READ | 3 << 8, 0),
                         Err(Error::Ownership { bits: 3 })));
        assert!(matches!(Entry::decode_raw(VALID | WRITE, 0), Err(Error::Permissions)));
        assert!(matches!(Entry::decode_raw(VALID | READ | 1 << 10, 2),
                         Err(Error::Physical { .. })));
        assert!(matches!(Entry::decode_raw(u64::MAX & !VALID, 0).unwrap(), Decoded::Invalid));
    }
    #[test]
    fn nonleaf_entries_preserve_owned_tables_and_reject_leaf_only_state()
    {
        let table = Entry::table(PhysicalAddress(4096), true, PageOwnership::Automatic).unwrap();
        assert!(matches!(table.decode(2).unwrap(),
                         Decoded::Table { physical:  PhysicalAddress(4096),
                                          global:    true,
                                          ownership: PageOwnership::Automatic }));
        assert!(matches!(table.decode(0), Err(Error::NonLeafAtBottom)));
        for flags in [USER, ACCESSED, DIRTY]
        {
            assert!(matches!(Entry::decode_raw(table.bits() | flags, 1),
                             Err(Error::ReservedBits { .. })));
        }
    }
}

impl mac_kernel_model::pages::Descriptor for Entry
{
    type Error = Error;

    fn invalid() -> Self
    {
        Self::invalid()
    }

    fn bits(self) -> u64
    {
        self.bits()
    }

    fn table(address: PhysicalAddress, global: bool, ownership: PageOwnership)
             -> Result<Self, Error>
    {
        Entry::table(address, global, ownership)
    }

    fn leaf(address: PhysicalAddress, size_bytes: u64, permissions: Permissions,
            ownership: PageOwnership)
            -> Result<Self, Error>
    {
        let size = match size_bytes
        {
            n if n == LeafSize::Page.bytes() => LeafSize::Page,
            n if n == LeafSize::Large.bytes() => LeafSize::Large,
            n if n == LeafSize::Huge.bytes() => LeafSize::Huge,
            _ => return Err(Error::VirtualAlignment { address:   address.0,
                                                      alignment: size_bytes })
        };
        Entry::leaf(address, size, permissions, ownership)
    }

    fn decode(bits: u64, level: u8) -> Result<mac_kernel_model::pages::EntryKind, Error>
    {
        Ok(match Entry::decode_raw(bits, level)?
        {
            Decoded::Invalid => mac_kernel_model::pages::EntryKind::Invalid,
            Decoded::Table { physical,
                             global,
                             ownership } =>
            {
                mac_kernel_model::pages::EntryKind::Table { physical,
                                                            global,
                                                            ownership }
            }
            Decoded::Leaf { physical,
                            size,
                            permissions,
                            ownership } =>
            {
                mac_kernel_model::pages::EntryKind::Leaf { physical,
                                                           size_bytes: size.bytes(),
                                                           permissions,
                                                           ownership }
            }
        })
    }
}
