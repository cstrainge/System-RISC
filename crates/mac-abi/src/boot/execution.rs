//! Data-only supervisor handoff. This validates layout facts, never ownership
//! of a live CPU root or arbitrary privileged-fault recovery. The architecture
//! boundary owns admission and keeps firmware storage/root backing pinned.
use core::fmt;

use super::{CheckedMemory, MemoryError, end};
use crate::BootMemoryRange;

pub const READ: u32 = 1;
pub const WRITE: u32 = 2;
pub const EXECUTE: u32 = 4;
pub const FIRMWARE_ONLY: u32 = 8;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Region
{
    pub physical:     BootMemoryRange,
    pub virtual_base: u64,
    pub flags:        u32,
    pub reserved:     u32
}
/// Raw wire data supplied by the native bootstrap. Plain array pointers retain
/// physical encodings until the architecture creates checked mapped views; no
/// opaque firmware object or existing Rust reference is rebased.
#[repr(C)]
pub struct Execution
{
    pub hart:              u64,
    pub root_physical:     u64,
    pub hardware_tag_mask: u64,
    pub physical_image:    BootMemoryRange,
    pub virtual_image:     BootMemoryRange,
    pub regions:           *const Region,
    pub region_count:      usize
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionError
{
    Flags
    {
        flags:    u32,
        reserved: u32
    },
    Span(MemoryError),
    FirmwareBacking,
    FirmwareIdentity,
    ImageOffset,
    WritableCode
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Memory(MemoryError),
    Root
    {
        physical: u64
    },
    Image,
    RegionCount
    {
        declared: usize,
        supplied: usize
    },
    Region
    {
        index: usize,
        cause: RegionError
    },
    Overlap
    {
        first:  usize,
        second: usize
    },
    MissingCode
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Memory(error) => error.fmt(f),
            Self::Root { physical } =>
            {
                write!(f,
                       "bootstrap translation root {physical:#x} is not a page-aligned reserved \
                        RAM frame")
            }
            Self::Image =>
            {
                f.write_str("bootstrap System physical/virtual extents disagree with the checked \
                             platform image reservation; rebuild ROM and System together")
            }
            Self::RegionCount { declared,
                                supplied } =>
            {
                write!(f,
                       "bootstrap declares {declared} mapping records but supplies {supplied}; a \
                        nonempty exact checked view is required")
            }
            Self::Region { index,
                           cause } =>
            {
                write!(f, "bootstrap mapping {index} is invalid: ")?;
                match cause
                {
                    RegionError::Flags { flags,
                                         reserved } =>
                    {
                        write!(f,
                               "permissions {flags:#x}/reserved {reserved:#x} contain unknown \
                                bits or write without read")
                    }
                    RegionError::Span(error) => error.fmt(f),
                    RegionError::FirmwareBacking =>
                    {
                        f.write_str("firmware bytes are outside reserved RAM or read-only \
                                     platform flash")
                    }
                    RegionError::FirmwareIdentity =>
                    {
                        f.write_str("opaque firmware storage requires its original physical CPU \
                                     address in the firmware root")
                    }
                    RegionError::ImageOffset =>
                    {
                        f.write_str("System physical and virtual mapping offsets must agree and \
                                     stay inside the image")
                    }
                    RegionError::WritableCode =>
                    {
                        f.write_str("System executable pages must not be writable")
                    }
                }
            }
            Self::Overlap { first,
                            second } =>
            {
                write!(f, "bootstrap mappings {first} and {second} overlap in virtual space")
            }
            Self::MissingCode => f.write_str("bootstrap System image has no executable mapping")
        }
    }
}
impl core::error::Error for Error
{
}
#[derive(Clone, Copy)]
pub struct Checked<'a>
{
    record:  &'a Execution,
    regions: &'a [Region]
}
fn contains(outer: BootMemoryRange, inner: BootMemoryRange) -> bool
{
    let Some(limit) = inner.base.checked_add(inner.length)
    else
    {
        return false;
    };
    outer.base <= inner.base && outer.base.checked_add(outer.length).is_some_and(|end| limit <= end)
}
impl Execution
{
    /// `regions` is a previously checked borrowed view, not a raw dereference.
    /// Platform/architecture callers validate canonical VA policy separately.
    pub fn checked<'a>(&'a self, memory: CheckedMemory<'_>, regions: &'a [Region],
                       physical_image: BootMemoryRange, virtual_image: BootMemoryRange,
                       flash: BootMemoryRange)
                       -> Result<Checked<'a>, Error>
    {
        let page = memory.page_bytes();
        // A zero window is a machine without flash, not an empty extent to
        // validate. Both encodings come from the same record convention, so this
        // is the absence case rather than a special one.
        if flash.length != 0
        {
            end(flash, page).map_err(Error::Memory)?;
        }
        if !self.root_physical.is_multiple_of(page)
           || !memory.reserved(BootMemoryRange { base:   self.root_physical,
                                                 length: page })
        {
            return Err(Error::Root { physical: self.root_physical });
        }
        end(physical_image, page).map_err(Error::Memory)?;
        end(virtual_image, page).map_err(Error::Memory)?;
        if self.physical_image != physical_image
           || self.virtual_image != virtual_image
           || physical_image.length != virtual_image.length
           || !memory.reserved(physical_image)
        {
            return Err(Error::Image);
        }
        if self.region_count != regions.len() || regions.is_empty()
        {
            return Err(Error::RegionCount { declared: self.region_count,
                                            supplied: regions.len() });
        }
        let mut code = false;
        for (index, region) in regions.iter().enumerate()
        {
            let refusal = |cause| {
                Error::Region { index,
                                cause }
            };
            if region.reserved != 0
               || region.flags & !(READ | WRITE | EXECUTE | FIRMWARE_ONLY) != 0
               || region.flags & (READ | EXECUTE) == 0
               || region.flags & WRITE != 0 && region.flags & READ == 0
            {
                return Err(refusal(RegionError::Flags { flags:    region.flags,
                                                        reserved: region.reserved }));
            }
            end(region.physical, page).map_err(|error| refusal(RegionError::Span(error)))?;
            let virtual_span = BootMemoryRange { base:   region.virtual_base,
                                                 length: region.physical.length };
            end(virtual_span, page).map_err(|error| refusal(RegionError::Span(error)))?;
            if region.flags & FIRMWARE_ONLY != 0
            {
                if region.virtual_base != region.physical.base
                {
                    return Err(refusal(RegionError::FirmwareIdentity));
                }
                if !memory.reserved(region.physical)
                   && !(contains(flash, region.physical) && region.flags & WRITE == 0)
                {
                    return Err(refusal(RegionError::FirmwareBacking));
                }
            }
            else
            {
                if !contains(physical_image, region.physical)
                   || !contains(virtual_image, virtual_span)
                   || region.physical.base - physical_image.base
                      != region.virtual_base - virtual_image.base
                {
                    return Err(refusal(RegionError::ImageOffset));
                }
                if region.flags & (WRITE | EXECUTE) == WRITE | EXECUTE
                {
                    return Err(refusal(RegionError::WritableCode));
                }
                code |= region.flags & EXECUTE != 0;
            }
            for (previous, old) in regions[..index].iter().enumerate()
            {
                if old.virtual_base < region.virtual_base + region.physical.length
                   && region.virtual_base < old.virtual_base + old.physical.length
                {
                    return Err(Error::Overlap { first:  previous,
                                                second: index });
                }
            }
        }
        if !code
        {
            return Err(Error::MissingCode);
        }
        Ok(Checked { record: self,
                     regions })
    }
}
impl<'a> Checked<'a>
{
    pub fn record(self) -> &'a Execution
    {
        self.record
    }

    pub fn regions(self) -> &'a [Region]
    {
        self.regions
    }
}
