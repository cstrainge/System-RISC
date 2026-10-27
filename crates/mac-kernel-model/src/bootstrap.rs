//! Allocation-free placement of each RAM bank's out-of-line buddy metadata.
//! The validated ROM account is the input. Plans contain address facts, never
//! references whose meaning would change during an MMU handoff.
use core::{alloc::Layout, fmt};

use mac_abi::{BootMemoryRange,
              boot::{CheckedMemory, FreeRanges}};

use crate::buddy::PageMetadata;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Bank
    {
        index: usize
    },
    Layout
    {
        bank: BootMemoryRange
    },
    NoStorage
    {
        bank:               BootMemoryRange,
        required_bytes:     u64,
        largest_free_bytes: u64
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Bank { index } => write!(f, "bootstrap RAM bank {index} does not exist"),
            Self::Layout { bank } =>
            {
                write!(f,
                       "buddy metadata layout for RAM {:#x}+{:#x} exceeds addressable storage",
                       bank.base, bank.length)
            }
            Self::NoStorage { bank,
                              required_bytes,
                              largest_free_bytes } =>
            {
                write!(f,
                       "RAM bank {:#x}+{:#x} needs {required_bytes} contiguous bytes of reserved \
                        buddy metadata; largest free span is {largest_free_bytes} bytes",
                       bank.base, bank.length)
            }
        }
    }
}
impl core::error::Error for Error
{
}

/// Offsets are relative to the reserved storage base, not to a CPU address.
/// The architecture supplies its own control-record layout; page metadata and
/// order heads use their actual Rust layouts instead of duplicated byte sizes.
#[derive(Clone, Copy, Debug)]
pub struct MetadataLayout
{
    pages:           usize,
    orders:          usize,
    metadata_offset: usize,
    heads_offset:    usize,
    bytes:           u64
}
impl MetadataLayout
{
    fn new(bank: BootMemoryRange, page_bytes: u64, control: Layout) -> Result<Self, Error>
    {
        let invalid = || Error::Layout { bank };
        let pages = usize::try_from(bank.length / page_bytes).map_err(|_| invalid())?;
        let orders = pages.ilog2() as usize + 1;
        let metadata = Layout::array::<PageMetadata>(pages).map_err(|_| invalid())?;
        let heads = Layout::array::<Option<usize>>(orders).map_err(|_| invalid())?;
        let (layout, metadata_offset) = control.extend(metadata).map_err(|_| invalid())?;
        let (layout, heads_offset) = layout.extend(heads).map_err(|_| invalid())?;
        // One page-aligned reservation covers all three non-overlapping views.
        if layout.align() as u64 > page_bytes
        {
            return Err(invalid());
        }
        let bytes = (layout.size() as u64).checked_add(page_bytes - 1)
                                          .map(|n| n / page_bytes * page_bytes)
                                          .ok_or_else(invalid)?;
        Ok(Self { pages,
                  orders,
                  metadata_offset,
                  heads_offset,
                  bytes })
    }

    pub const fn pages(self) -> usize
    {
        self.pages
    }

    pub const fn orders(self) -> usize
    {
        self.orders
    }

    pub const fn metadata_offset(self) -> usize
    {
        self.metadata_offset
    }

    pub const fn heads_offset(self) -> usize
    {
        self.heads_offset
    }

    pub const fn bytes(self) -> u64
    {
        self.bytes
    }
}

/// A placement plan is not an allocation lease and cannot publish raw access.
/// The native bootstrap must reserve it and create an exclusive initialized
/// view before constructing the pool. Plans for separate banks never overlap.
pub struct BankPlan<'a>
{
    bank:    BootMemoryRange,
    storage: BootMemoryRange,
    layout:  MetadataLayout,
    free:    FreeRanges<'a>
}
impl<'a> BankPlan<'a>
{
    /// A wholly reserved bank needs no pool or metadata and returns None.
    pub fn new(memory: CheckedMemory<'a>, index: usize, control: Layout)
               -> Result<Option<Self>, Error>
    {
        let bank = *memory.banks().get(index).ok_or(Error::Bank { index })?;
        let free = memory.free_ranges(index).ok_or(Error::Bank { index })?;
        let Some(largest) = free.clone().max_by_key(|range| range.length)
        else
        {
            return Ok(None);
        };
        let layout = MetadataLayout::new(bank, memory.page_bytes(), control)?;
        let storage = free.clone()
                          .find(|range| range.length >= layout.bytes)
                          .map(|range| {
                              BootMemoryRange { base:   range.base,
                                                length: layout.bytes }
                          })
                          .ok_or(Error::NoStorage { bank,
                                                    required_bytes: layout.bytes,
                                                    largest_free_bytes: largest.length })?;
        Ok(Some(Self { bank,
                       storage,
                       layout,
                       free }))
    }

    pub const fn bank(&self) -> BootMemoryRange
    {
        self.bank
    }

    pub const fn storage(&self) -> BootMemoryRange
    {
        self.storage
    }

    pub const fn layout(&self) -> MetadataLayout
    {
        self.layout
    }

    pub fn free_ranges(&self) -> PoolFreeRanges<'a>
    {
        PoolFreeRanges { source:  self.free.clone(),
                         storage: self.storage,
                         pending: None }
    }
}
/// Subtract metadata from the original validated ROM exclusions. No copied
/// bitmap, merged second reservation list, or per-physical-page scan exists.
#[derive(Clone)]
pub struct PoolFreeRanges<'a>
{
    source:  FreeRanges<'a>,
    storage: BootMemoryRange,
    pending: Option<BootMemoryRange>
}
impl Iterator for PoolFreeRanges<'_>
{
    type Item = BootMemoryRange;

    fn next(&mut self) -> Option<Self::Item>
    {
        if let Some(range) = self.pending.take()
        {
            return Some(range);
        }
        loop
        {
            let range = self.source.next()?;
            if range.base <= self.storage.base && self.storage.base < range.base + range.length
            {
                let end = self.storage.base + self.storage.length;
                if end < range.base + range.length
                {
                    self.pending = Some(BootMemoryRange { base:   end,
                                                          length: range.base + range.length - end });
                }
                if range.base < self.storage.base
                {
                    return Some(BootMemoryRange { base:   range.base,
                                                  length: self.storage.base - range.base });
                }
                if let Some(range) = self.pending.take()
                {
                    return Some(range);
                }
            }
            else
            {
                return Some(range);
            }
        }
    }
}
impl core::iter::FusedIterator for PoolFreeRanges<'_>
{
}
