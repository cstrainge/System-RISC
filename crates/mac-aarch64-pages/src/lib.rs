//! AArch64 descriptor encoding, separate from physical ownership and CPU
//! instructions.
//!
//! This is the peer of `mac-riscv-pages`: the same public surface, a different
//! descriptor layout. Shared paging stores and decodes descriptors through
//! `machine::page_layout`, so it never names a format; which of these two crates
//! answers is decided by the target the platform record declares.
//!
//! The layout follows docs/AARCH64-PAGING.md. No pointer dereference, register
//! write or implicit activation exists in this crate.
#![no_std]
#![forbid(unsafe_code)]
pub mod mapping;
pub mod walk;
use core::fmt;

use mac_kernel_model::pages::{Access, PageOwnership, Permissions, PhysicalAddress};

/// Valid descriptor.
const VALID: u64 = 1 << 0;
/// Set on a block or page; clear on a table pointer.
const LEAF: u64 = 1 << 1;
/// Access flag. An unset flag faults on first access.
const AF: u64 = 1 << 10;
/// Inner shareable, used for normal memory.
const SH_INNER: u64 = 0b11 << 8;
/// Attribute index 0 in `MAIR_EL1`: normal, write-back.
const ATTR_NORMAL: u64 = 0;
/// Attribute index 1 in `MAIR_EL1`: Device-nGnRnE, in `AttrIndx` (bits 4:2).
const ATTR_DEVICE: u64 = 1 << 2;
/// The `AttrIndx` field.
const ATTR_MASK: u64 = 0b111 << 2;
/// Privileged execute-never.
const PXN: u64 = 1 << 53;
/// Unprivileged execute-never.
const UXN: u64 = 1 << 54;
/// The output address occupies bits 47:12 for the physical size this port uses.
const OUTPUT_MASK: u64 = ((1 << 48) - 1) & !0xfff;
/// Software-owned bits. They are architectural only above a 48-bit physical
/// address, so a port that stays within 48 bits may use them.
const SOFTWARE_SHIFT: u32 = 55;
const SOFTWARE_MASK: u64 = 0b11 << SOFTWARE_SHIFT;

/// The base page this format is built on.
pub const PAGE_BYTES: u64 = 4096;
/// The leaves, coarsest first: 1 GiB block, 2 MiB block, 4 KiB page.
///
/// The order is part of the contract, not a presentation choice:
/// `MappingPlan::new` requires a strictly descending list whose last entry is the
/// base page, so that a span is split by taking the largest leaf that fits. This
/// list was finest-first, which made every mapping fail with `InvalidGeometry` -
/// and the correct order was sitting beside it in a dead constant that nothing
/// read. There is now one list, and a test in this crate pins the order.
pub const LEAF_BYTES: &[u64] = &[1 << 30, 1 << 21, 1 << 12];
/// The top of the lower half of a 39-bit space.
const LOWER_END: u64 = 0x0000_0080_0000_0000;
/// The bottom of the upper half.
const UPPER_START: u64 = 0xffff_ff80_0000_0000;

pub fn geometry() -> mac_kernel_model::pages::PageGeometry
{
    mac_kernel_model::pages::PageGeometry::new(PAGE_BYTES)
        .expect("the aarch64 base page geometry is 4096 bytes")
}

pub fn leaf_sizes() -> &'static [u64]
{
    LEAF_BYTES
}

/// A range is valid when it lies wholly inside one canonical half.
pub fn valid_virtual_range(range: mac_kernel_model::policy::AddressRange) -> bool
{
    let (start, end) = (range.start(), range.end());
    (start < LOWER_END && end <= LOWER_END) || start >= UPPER_START
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    NonCanonical
    {
        address: u64
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
    Reserved
    {
        bits: u64
    }
}

impl fmt::Display for Error
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::NonCanonical { address } => write!(out, "non-canonical address {address:#x}"),
            Self::Level { level } => write!(out, "no leaf at level {level}"),
            Self::Physical { address,
                             alignment } =>
            {
                write!(out, "physical address {address:#x} is not {alignment:#x} aligned")
            }
            Self::Permissions => out.write_str("unsupported permission combination"),
            Self::Reserved { bits } => write!(out, "reserved descriptor bits set: {bits:#x}")
        }
    }
}

/// Leaves counted from the finest, which is the shared convention: 0 is the
/// 4 KiB page. The architecture's own numbering is available through
/// `table_level`.
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
        match self
        {
            Self::Page => 1 << 12,
            Self::Large => 1 << 21,
            Self::Huge => 1 << 30
        }
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

    pub const fn from_level(level: u8) -> Result<Self, Error>
    {
        match level
        {
            0 => Ok(Self::Page),
            1 => Ok(Self::Large),
            2 => Ok(Self::Huge),
            other => Err(Error::Level { level: other })
        }
    }

    /// The architecture's own table level: the walk starts at 1 and the 4 KiB
    /// leaf is level 3.
    pub const fn table_level(self) -> u8
    {
        match self
        {
            Self::Page => 3,
            Self::Large => 2,
            Self::Huge => 1
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
        PageOwnership::Automatic => 1 << SOFTWARE_SHIFT,
        PageOwnership::CopyOnWrite => 2 << SOFTWARE_SHIFT
    }
}

fn ownership_from(bits: u64) -> PageOwnership
{
    match (bits & SOFTWARE_MASK) >> SOFTWARE_SHIFT
    {
        1 => PageOwnership::Automatic,
        2 => PageOwnership::CopyOnWrite,
        _ => PageOwnership::Manual
    }
}

fn output_bits(address: PhysicalAddress, alignment: u64) -> Result<u64, Error>
{
    if address.0 >= 1 << 48 || !address.0.is_multiple_of(alignment)
    {
        return Err(Error::Physical { address: address.0,
                                     alignment });
    }
    Ok(address.0 & OUTPUT_MASK)
}

fn output_address(bits: u64) -> PhysicalAddress
{
    PhysicalAddress(bits & OUTPUT_MASK)
}

/// The permissions a leaf descriptor encodes. Stated once, so the block and
/// page branches cannot disagree about which bit means what.
fn permissions_from(bits: u64) -> Permissions
{
    // `AP[1]` (bit 6) grants EL0 access when set; `AP[2]` (bit 7) is the
    // read-only bit. A page executes only when both `PXN` and `UXN` are clear.
    Permissions { device:  bits & ATTR_MASK == ATTR_DEVICE,
                  read:    true,
                  write:   bits & (1 << 7) == 0,
                  execute: bits & (PXN | UXN) == 0,
                  access:  if bits & (1 << 6) != 0
                  {
                      Access::User
                  }
                  else
                  {
                      Access::Supervisor
                  },
                  global:  bits & (1 << 11) == 0 }
}

/// One encoded descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry(u64);

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
        // A table pointer is 4 KiB aligned and carries bit 1, which is what
        // separates it from a block: clear at this level means a block that maps
        // its own size, so a table written without it is read by hardware as a
        // block over the table's own page. AArch64 has no global bit on a table
        // descriptor, so `global` only records the caller's intent for the
        // entries that table will hold.
        let _ = global;
        Ok(Self(output_bits(address, 1 << 12)? | VALID | LEAF | ownership_bits(ownership)))
    }

    pub fn leaf(address: PhysicalAddress, size: LeafSize, permissions: Permissions,
                ownership: PageOwnership)
                -> Result<Self, Error>
    {
        if (!permissions.read && !permissions.execute)
           || (permissions.write && !permissions.read)
           || (ownership == PageOwnership::CopyOnWrite && permissions.write)
           || (permissions.device && permissions.execute)
        {
            return Err(Error::Permissions);
        }
        // `AP[2:1]` (bits 7:6): `0b01`/`0b11` grant EL0 access and
        // `0b00`/`0b10` keep the page to EL1. Bit 6 is `AP[1]`, the
        // unprivileged-access bit, so it is set for an application page and
        // clear for a supervisor page.
        let ap1 = if permissions.access == Access::User { 1 << 6 } else { 0 };
        // `AP[2]` (bit 7) is the read-only bit: set means read-only.
        let ap2 = if permissions.write { 0 } else { 1 << 7 };
        let never_execute = if permissions.execute { 0 } else { PXN | UXN };
        // Bit 1 separates a table pointer from a leaf only at the page level.
        // A block descriptor at a coarser level must have it clear: hardware
        // reads a block with bit 1 set as a table pointer and walks to the
        // output address as if a table were there, which faults rather than
        // mapping anything.
        let leaf = if size == LeafSize::Page { LEAF } else { 0 };
        // Registers are never cached, merged or reordered; memory is.
        let attribute = if permissions.device { ATTR_DEVICE } else { ATTR_NORMAL };
        Ok(Self(output_bits(address, size.bytes())?
                 | VALID
                 | leaf
                 | AF
                 | SH_INNER
                 | attribute
                 | ap1
                 | ap2
                 | never_execute
                 | ownership_bits(ownership)))
    }

    pub fn decode_raw(bits: u64, level: u8) -> Result<Decoded, Error>
    {
        let size = LeafSize::from_level(level)?;
        if bits & VALID == 0
        {
            return Ok(Decoded::Invalid);
        }
        // Anything outside the fields this port writes is a descriptor the
        // decoder does not understand, and it is refused rather than accepted.
        let known = OUTPUT_MASK | SOFTWARE_MASK | VALID | LEAF | AF | SH_INNER | (3 << 6)
                    | (1 << 11) | PXN | UXN
                    // The one memory type besides normal that this port writes. Any
                    // other attribute index is still a descriptor nobody encoded.
                    | ATTR_DEVICE;
        let reserved = bits & !known;
        if reserved != 0
        {
            return Err(Error::Reserved { bits: reserved });
        }
        if bits & LEAF == 0
        {
            // At the page level bit 1 must be set; a page descriptor without it
            // maps nothing. Above it, bit 1 clear is a block, not a table: a
            // block is a leaf that maps `size` bytes, and only bit 1 set makes
            // the entry a pointer to the next level.
            if size == LeafSize::Page
            {
                return Err(Error::Level { level });
            }
            return Ok(Decoded::Leaf { physical: output_address(bits),
                                      size,
                                      permissions: permissions_from(bits),
                                      ownership: ownership_from(bits) });
        }
        if size != LeafSize::Page
        {
            return Ok(Decoded::Table { physical:  output_address(bits),
                                       global:    bits & (1 << 11) == 0,
                                       ownership: ownership_from(bits) });
        }
        Ok(Decoded::Leaf { physical:    output_address(bits),
                           size,
                           permissions: permissions_from(bits),
                           ownership:   ownership_from(bits) })
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
            _ => return Err(Error::Physical { address:   address.0,
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

