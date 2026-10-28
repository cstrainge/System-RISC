//! One physical heap lease, partitioned before either consumer sees storage.
//! Protected ranges are permanent, unique capabilities; they never own a second
//! buddy lease or return storage to the ordinary allocator.
use core::{fmt,
           sync::atomic::{AtomicUsize, Ordering}};

use mac_kernel_model::pages::PhysicalAddress;

use super::super::{memory::*, pages::Backing};

#[derive(Debug)]
pub(crate) enum Error
{
    Layout,
    Exhausted
    {
        requested: usize,
        remaining: usize
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Layout =>
            {
                out.write_str("System heap partition violates its checked page-aligned capacity")
            }
            Self::Exhausted { requested,
                              remaining } =>
            {
                write!(out,
                       "protected System heap refused {requested} bytes with {remaining} bytes \
                        remaining")
            }
        }
    }
}

pub(super) struct HeapPartition
{
    backing: Backing,
    used:    AtomicUsize
}
#[derive(Clone, Copy)]
pub(crate) struct ProtectedHeapStatistics
{
    pub(crate) cpu_base:           usize,
    pub(crate) physical_base:      u64,
    pub(crate) capacity_bytes:     usize,
    pub(crate) used_bytes:         usize,
    pub(crate) full_backing_bytes: u64
}
impl HeapPartition
{
    pub(super) fn new(backing: Backing) -> Result<Self, Error>
    {
        if SYSTEM_RECOVERY_BYTES == 0
           || SYSTEM_RECOVERY_BYTES >= SYSTEM_HEAP_BYTES
           || !SYSTEM_RECOVERY_BYTES.is_multiple_of(PAGE_BYTES as usize)
           || !SYSTEM_HEAP_BYTES.is_multiple_of(PAGE_BYTES as usize)
           || VM_SYSTEM_HEAP_LIMIT.checked_sub(VM_SYSTEM_HEAP_BASE)
              != Some(SYSTEM_HEAP_BYTES as u64)
           || backing.bytes() < SYSTEM_HEAP_BYTES as u64
           || backing.physical().0.checked_add(backing.bytes()).is_none()
        {
            return Err(Error::Layout);
        }
        Ok(Self { backing,
                  used: AtomicUsize::new(0) })
    }

    pub(super) fn physical(&self) -> PhysicalAddress
    {
        self.backing.physical()
    }

    pub(super) fn bytes(&self) -> u64
    {
        self.backing.bytes()
    }

    pub(super) fn ordinary_range(&self) -> (usize, usize)
    {
        (VM_SYSTEM_HEAP_BASE as usize, SYSTEM_HEAP_BYTES - SYSTEM_RECOVERY_BYTES)
    }

    pub(super) fn statistics(&self) -> ProtectedHeapStatistics
    {
        let offset = SYSTEM_HEAP_BYTES - SYSTEM_RECOVERY_BYTES;
        ProtectedHeapStatistics { cpu_base:           VM_SYSTEM_HEAP_BASE as usize + offset,
                                  physical_base:      self.backing.physical().0 + offset as u64,
                                  capacity_bytes:     SYSTEM_RECOVERY_BYTES,
                                  used_bytes:         self.used.load(Ordering::Acquire),
                                  full_backing_bytes: self.backing.bytes() }
    }

    // Only Spaces publishes this owner after both roots map the entire extent.
    // The owner and every returned capability remain pinned until reset. A
    // dropped capability intentionally does not reclaim terminal storage.
    fn reserve_zeroed(&'static self, bytes: usize) -> Result<ProtectedRange, Error>
    {
        if bytes == 0 || !bytes.is_multiple_of(PAGE_BYTES as usize)
        {
            return Err(Error::Layout);
        }
        let offset = self.used
                         .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                             used.checked_add(bytes).filter(|&end| end <= SYSTEM_RECOVERY_BYTES)
                         })
                         .map_err(|used| {
                             Error::Exhausted { requested: bytes,
                                                remaining:
                                                    SYSTEM_RECOVERY_BYTES.saturating_sub(used) }
                         })?;
        let range = ProtectedRange { owner: self,
                                     offset,
                                     bytes };
        // SAFETY: the atomic bump grants one disjoint range inside the retained
        // Backing. Both permanent roots already map it supervisor RW/NX at this
        // stable address. No typed view exists yet; initialize only this range.
        unsafe {
            core::ptr::write_bytes(range.cpu_base() as *mut u8, 0, bytes);
        }
        Ok(range)
    }

    pub(super) fn terminal_storage(&'static self, frame: usize, stack: usize)
                                   -> Result<(ProtectedRange, ProtectedRange), Error>
    {
        let page = PAGE_BYTES as usize;
        let round = |bytes: usize| {
            bytes.checked_add(page - 1)
                 .map(|n| n / page * page)
                 .filter(|&n| n != 0)
                 .ok_or(Error::Layout)
        };
        let frame = round(frame)?;
        let stack = round(stack)?;
        let total = frame.checked_add(stack).ok_or(Error::Layout)?;
        // One reservation makes capacity refusal atomic for the pair.
        self.reserve_zeroed(total)?.split_at(frame)
    }
}

/// A unique initialized subrange borrowed from the reset-lifetime heap owner.
/// Numeric accessors do not confer additional physical ownership. No Clone,
/// mutable slice, reference rebasing, independent free or buddy token exists.
pub(crate) struct ProtectedRange
{
    owner:  &'static HeapPartition,
    offset: usize,
    bytes:  usize
}
impl ProtectedRange
{
    fn split_at(self, bytes: usize) -> Result<(Self, Self), Error>
    {
        if bytes == 0 || bytes >= self.bytes || !bytes.is_multiple_of(PAGE_BYTES as usize)
        {
            return Err(Error::Layout);
        }
        Ok((Self { owner: self.owner,
                   offset: self.offset,
                   bytes },
            Self { owner:  self.owner,
                   offset: self.offset + bytes,
                   bytes:  self.bytes - bytes }))
    }

    pub(crate) fn cpu_base(&self) -> usize
    {
        VM_SYSTEM_HEAP_BASE as usize + SYSTEM_HEAP_BYTES - SYSTEM_RECOVERY_BYTES + self.offset
    }

    pub(crate) fn physical(&self) -> PhysicalAddress
    {
        PhysicalAddress(self.owner.physical().0
                        + (SYSTEM_HEAP_BYTES - SYSTEM_RECOVERY_BYTES + self.offset) as u64)
    }

    pub(crate) fn bytes(&self) -> u64
    {
        self.bytes as u64
    }

    pub(crate) fn top(&self) -> usize
    {
        self.cpu_base() + self.bytes
    }
}
