//! The memory account the loader hands to the kernel.
//!
//! Two lists, both checked before publication: the RAM banks the machine
//! describes, and the extents the kernel's page pool must not take. Everything
//! in the second list is a region this boot actually occupies or reserves, so a
//! kernel that trusted an incomplete list would hand live firmware memory to a
//! process.
use alloc::vec::Vec;

use mac_abi::BootMemoryRange;

use mac_devicetree::Tree;

/// A page, from the record rather than assumed.
pub const PAGE_BYTES: u64 = 4096;

#[derive(Debug)]
pub enum Error
{
    /// The machine's tree describes no RAM at all.
    NoRam,
    /// A bank is not page aligned, so it cannot be described to the kernel.
    Unaligned,
    /// Two banks overlap, so the account would double-count the same bytes.
    Overlap,
    /// A reservation is empty, unaligned or outside every bank.
    Reservation,
    /// Two reservations overlap, so one claim would cover two regions.
    Duplicate,
    /// The machine's tree does not lie inside the RAM it describes.
    TreeOutsideRam,
    /// Metadata allocation failed.
    Allocation,
    /// The machine's tree could not be walked.
    TreeUnreadable
}

pub struct Memory
{
    /// Banks, primary first, then ascending by base.
    pub ram:      Vec<BootMemoryRange>,
    /// Disjoint extents the page pool must not take.
    pub reserved: Vec<BootMemoryRange>
}

fn end(range: &BootMemoryRange) -> Option<u64>
{
    range.base.checked_add(range.length).filter(|_| range.length != 0)
}

fn wire(base: u64, length: u64) -> BootMemoryRange
{
    BootMemoryRange { base, length }
}

impl Memory
{
    /// Discovers the machine's RAM and reserves what this boot occupies.
    ///
    /// The device tree is reserved where the machine actually put it rather
    /// than at a fixed offset. The translation tables need no entry of their
    /// own: `link.ld` places them inside the boot window, which is reserved.
    pub fn discover(tree: &Tree<'_>, tree_base: u64, tree_bytes: u64) -> Result<Self, Error>
    {
        let mut ram = Vec::new();
        tree.memory(|base, bytes| {
              if base.is_multiple_of(PAGE_BYTES) && bytes.is_multiple_of(PAGE_BYTES)
              {
                  ram.push(wire(base, bytes));
              }
          }).map_err(|_| Error::TreeUnreadable)?;
        if ram.is_empty()
        {
            return Err(Error::NoRam);
        }
        ram.sort_unstable_by_key(|bank| bank.base);
        for pair in ram.windows(2)
        {
            if end(&pair[0]).is_none_or(|limit| limit > pair[1].base)
            {
                return Err(Error::Overlap);
            }
        }
        // Keep the bank the loader runs in first: the kernel's summary fields
        // name it, and every other bank is still described.
        let index = ram.iter()
                       .position(|bank| {
                           bank.base <= crate::PHYSICAL_RAM_BASE
                           && end(bank).is_some_and(|limit| {
                               crate::PHYSICAL_RAM_BASE + crate::PHYSICAL_RAM_BYTES <= limit
                           })
                       })
                       .ok_or(Error::NoRam)?;
        ram.swap(0, index);

        let mut memory = Self { ram,
                                reserved: Vec::new() };
        for (base, bytes) in [(crate::BOOT_BASE, crate::BOOT_BYTES),
                              (crate::IMAGE_BASE, crate::IMAGE_BYTES),
                              (crate::FW_HEAP_BASE, crate::FW_HEAP_BYTES),
                              (crate::DISPLAY_SCRATCH_BASE, crate::DISPLAY_SCRATCH_BYTES),
                              // Everything below the boot window. The firmware's
                              // start-up stub lives there and the three cores it
                              // has not released are spinning in it, each waiting
                              // on a word in that page: a pool that handed the
                              // page out would let an ordinary store release a
                              // core into whatever was written.
                              (crate::PHYSICAL_RAM_BASE,
                               crate::BOOT_BASE - crate::PHYSICAL_RAM_BASE)]
        {
            if bytes != 0
            {
                memory.reserve(base, bytes)?;
            }
        }
        // The machine's own description, at the address the machine used. The
        // firmware does not page align it, and a reservation is whole pages, so
        // the extent is widened to the pages the blob touches.
        let first = tree_base - tree_base % PAGE_BYTES;
        let limit = tree_base.checked_add(tree_bytes)
                             .ok_or(Error::TreeOutsideRam)?
                             .next_multiple_of(PAGE_BYTES);
        memory.reserve(first, limit - first)?;
        Ok(memory)
    }

    fn contains(&self, base: u64, bytes: u64) -> bool
    {
        self.ram.iter()
                .any(|bank| bank.base <= base
                            && end(bank).is_some_and(|limit| base + bytes <= limit))
    }

    fn reserve(&mut self, base: u64, bytes: u64) -> Result<(), Error>
    {
        if bytes == 0
           || !base.is_multiple_of(PAGE_BYTES)
           || !bytes.is_multiple_of(PAGE_BYTES)
           || !self.contains(base, bytes)
        {
            return Err(Error::Reservation);
        }
        let limit = base + bytes;
        if self.reserved.iter().any(|other| {
                                     other.base < limit
                                     && base < other.base + other.length
                                 })
        {
            return Err(Error::Duplicate);
        }
        self.reserved.try_reserve(1).map_err(|_| Error::Allocation)?;
        self.reserved.push(wire(base, bytes));
        Ok(())
    }

    /// The account the kernel validates against, borrowing both lists.
    pub fn checked(&self, page_bytes: u64) -> Result<mac_abi::boot::CheckedMemory<'_>,
                                                     mac_abi::boot::MemoryError>
    {
        mac_abi::boot::Memory { ram:          &self.ram,
                                reservations: &self.reserved }.checked(page_bytes, self.ram[0])
    }
}
