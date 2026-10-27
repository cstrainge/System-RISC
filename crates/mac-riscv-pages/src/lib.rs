//! RV64 Sv39 encoding, separate from physical ownership and CPU instructions.
//! Architecture reference: RISC-V Privileged ISA, Supervisor chapter, Sv39.
//! No pointer dereference, satp write or implicit activation exists in this crate.
#![no_std]
#![forbid(unsafe_code)]
pub mod mapping;
use core::fmt;

use mac_kernel_model::{pages::{Access, PageOwnership, Permissions, PhysicalAddress},
                       policy::AddressRange};

pub struct Sv39;
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
        Ok(((address >> (size.shift())) & 511) as usize)
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
    NonLeafAtBottom
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
                       "Sv39 physical address {address:#x} must fit56 bits and be aligned to \
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
                f.write_str("Sv39 level0 entry cannot refer to a lower page table")
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
