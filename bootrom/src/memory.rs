//! Bootstrap reservation account. Build policy and discovered extents stay distinct.
#![forbid(unsafe_code)]
use alloc::vec::Vec;
use core::{fmt, ops::Range};

use mac_abi::BootMemoryRange;

use crate::devicetree::{Error, Tree};

include!(concat!(env!("OUT_DIR"), "/platform-memory.rs"));

pub struct Memory
{
    pub ram:      Vec<BootMemoryRange>,
    pub reserved: Vec<BootMemoryRange>
}
fn primary(tree: &Tree<'_>) -> Result<Range<u64>, Error>
{
    let mut primary = None;
    tree.memory(|range| {
            if range.start == RAM_BASE
            {
                if primary.replace(range).is_some()
                {
                    return Err(Error::Malformed("duplicate primary RAM bank in device tree"));
                }
            }
            Ok(())
        })?;
    let primary =
        primary.ok_or(Error::Malformed("device tree has no RAM bank at the platform RAM base"))?;
    if primary.end - primary.start < MINIMUM_RAM_BYTES
    {
        return Err(Error::Malformed("discovered RAM is smaller than the platform minimum; \
                                     increase VM RAM or select another platform"));
    }
    Ok(primary)
}
/// Allocation-free admission runs before touching the firmware heap.
pub fn admit(tree: &Tree<'_>) -> Result<(), Error>
{
    primary(tree).map(|_| ())
}
/// CPU metadata admission keeps format/allocation refusal separate from the
/// checked topology's exact identity failure. Neither category publishes data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuDiscoveryError
{
    Format(Error),
    Topology(mac_abi::boot::topology::Error)
}
impl fmt::Display for CpuDiscoveryError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Format(cause) => cause.fmt(out),
            Self::Topology(cause) => cause.fmt(out)
        }
    }
}
impl From<Error> for CpuDiscoveryError
{
    fn from(cause: Error) -> Self
    {
        Self::Format(cause)
    }
}
impl From<mac_abi::boot::topology::Error> for CpuDiscoveryError
{
    fn from(cause: mac_abi::boot::topology::Error) -> Self
    {
        Self::Topology(cause)
    }
}
/// Allocate topology only after the memory preflight made firmware heap use
/// safe. No CPU is released; sorted wire IDs are immutable discovery metadata.
pub fn discover_cpus(tree: &Tree<'_>, primary: u64)
                     -> Result<Vec<mac_abi::boot::topology::Cpu>, CpuDiscoveryError>
{
    let count = tree.cpu_count(primary)?;
    let mut cpus = Vec::new();
    cpus.try_reserve_exact(count)
        .map_err(|_| Error::AllocationFailed("allocation failed for enabled CPU metadata"))?;
    tree.cpus(|cpu| {
            cpus.push(cpu);
            Ok(())
        })?;
    cpus.sort_unstable_by_key(|cpu| cpu.hart);
    mac_abi::boot::topology::Topology::checked(&cpus, primary)?;
    Ok(cpus)
}
fn wire(range: Range<u64>) -> BootMemoryRange
{
    BootMemoryRange { base:   range.start,
                      length: range.end - range.start }
}
fn end(range: &BootMemoryRange) -> Result<u64, Error>
{
    range.base.checked_add(range.length).ok_or(Error::Malformed("boot memory extent overflows"))
}
impl Memory
{
    pub fn discover(tree: &Tree<'_>, tree_address: u64) -> Result<Self, Error>
    {
        let primary = primary(tree)?;
        let mut ram = Vec::new();
        tree.memory(|range| {
                if !range.start.is_multiple_of(PAGE_BYTES) || !range.end.is_multiple_of(PAGE_BYTES)
                {
                    return Err(Error::Malformed("device tree RAM bank is not page aligned"));
                }
                ram.try_reserve(1)
                   .map_err(|_| {
                       Error::AllocationFailed("allocation failed for discovered RAM metadata")
                   })?;
                ram.push(wire(range));
                Ok(())
            })?;
        ram.sort_unstable_by_key(|range| range.base);
        for banks in ram.windows(2)
        {
            if end(&banks[0])? > banks[1].base
            {
                return Err(Error::Malformed("device tree RAM banks overlap"));
            }
        }
        // Keep the primary first for the existing summary fields. No bank is lost.
        let index = ram.iter().position(|range| range.base == primary.start).unwrap();
        ram.swap(0, index);
        let mut memory = Self { ram,
                                reserved: Vec::new() };
        let tail = primary.end
                          .checked_sub(FDT_TAIL_BYTES)
                          .ok_or(Error::Malformed("device tree reservation exceeds RAM"))?
                   ..primary.end;
        let tree_end = tree_address.checked_add(tree.total_size() as u64)
                                   .ok_or(Error::Malformed("device tree address overflows"))?;
        if tree_address < tail.start || tree_end > tail.end
        {
            return Err(Error::Malformed("device tree is outside the platform's reserved RAM \
                                         tail"));
        }
        for (base, limit) in BOOT_RESERVED
        {
            memory.reserve(base..limit)?;
        }
        memory.reserve(tail)?;
        // Platform reservations must not silently overwrite unknown firmware data.
        // An already-covered reservation describes the same unavailable bytes.
        tree.reservations(|range| {
                let start = range.start & !(PAGE_BYTES - 1);
                let end =
                    range.end
                         .checked_add(PAGE_BYTES - 1)
                         .ok_or(Error::Malformed("device tree reservation alignment overflows"))?
                    & !(PAGE_BYTES - 1);
                memory.reserve_union(start..end)
            })?;
        Ok(memory)
    }

    fn contains(&self, range: &Range<u64>) -> bool
    {
        self.ram.iter().any(|bank| {
                           bank.base <= range.start
                           && bank.base.checked_add(bank.length).is_some_and(|end| end >= range.end)
                       })
    }

    fn reserve_union(&mut self, mut range: Range<u64>) -> Result<(), Error>
    {
        if !self.contains(&range)
        {
            return Err(Error::Malformed("device tree reservation lies outside discovered RAM"));
        }
        // Merge excluded bytes only; this never authorizes an image write.
        let mut index = 0;
        while index < self.reserved.len()
        {
            let previous = self.reserved[index];
            let limit = end(&previous)?;
            if previous.base <= range.end && range.start <= limit
            {
                range.start = range.start.min(previous.base);
                range.end = range.end.max(limit);
                self.reserved.swap_remove(index);
            }
            else
            {
                index += 1;
            }
        }
        self.reserve(range)
    }

    pub fn reserve(&mut self, range: Range<u64>) -> Result<(), Error>
    {
        if range.start >= range.end
           || !range.start.is_multiple_of(PAGE_BYTES)
           || !range.end.is_multiple_of(PAGE_BYTES)
           || !self.contains(&range)
        {
            return Err(Error::Malformed("boot reservation is empty, unaligned or outside \
                                         discovered RAM"));
        }
        if self.reserved
               .iter()
               .any(|other| range.start < other.base + other.length && other.base < range.end)
        {
            return Err(Error::Malformed("loader image overlaps an existing boot reservation"));
        }
        self.reserved
            .try_reserve(1)
            .map_err(|_| {
                Error::AllocationFailed("allocation failed for boot reservation metadata")
            })?;
        self.reserved.push(wire(range));
        Ok(())
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn real_policy_preserves_discovered_size_and_disjoint_reservations()
    {
        let bytes = crate::devicetree::tests::blob(&[(RAM_BASE, 0x200000000)], false);
        let tree = Tree::new(&bytes).unwrap();
        let mut memory = Memory::discover(&tree, RAM_BASE + 0x200000000 - FDT_TAIL_BYTES).unwrap();
        assert_eq!(memory.ram[0].length, 0x200000000);
        // Firmware's device-tree reservation is covered by the platform's
        // existing exclusion, never counted twice or treated as free bytes.
        for &(start, end) in &BOOT_RESERVED
        {
            assert!(memory.reserved
                          .iter()
                          .any(|range| range.base <= start && range.base + range.length >= end));
        }
        assert!(memory.reserved.iter().any(|range| {
                                          range.base <= DISPLAY_SCRATCH_BASE
                                          && range.base + range.length
                                             >= DISPLAY_SCRATCH_BASE + DISPLAY_SCRATCH_BYTES
                                      }));
        memory.reserve(IMAGE_BASE..IMAGE_BASE + PAGE_BYTES).unwrap();
        mac_abi::boot::Memory { ram:          &memory.ram,
                                reservations: &memory.reserved }.validate(PAGE_BYTES,
                                                                          memory.ram[0])
                                                                .unwrap();
        assert_eq!(memory.reserve(IMAGE_BASE..IMAGE_BASE + PAGE_BYTES),
                   Err(Error::Malformed("loader image overlaps an existing boot reservation")));
        assert_eq!(memory.reserve(0..PAGE_BYTES),
                   Err(Error::Malformed("boot reservation is empty, unaligned or outside \
                                         discovered RAM")));
    }
    #[test]
    fn admission_refuses_small_ram_before_heap_use_and_wrong_fdt_placement()
    {
        let bytes = crate::devicetree::tests::blob(&[(RAM_BASE, MINIMUM_RAM_BYTES / 2)], false);
        let tree = Tree::new(&bytes).unwrap();
        assert_eq!(admit(&tree),
                   Err(Error::Malformed("discovered RAM is smaller than the platform minimum; \
                                         increase VM RAM or select another platform")));
        let bytes = crate::devicetree::tests::blob(&[(RAM_BASE, MINIMUM_RAM_BYTES)], false);
        let tree = Tree::new(&bytes).unwrap();
        assert!(matches!(Memory::discover(&tree, IMAGE_BASE),
                         Err(Error::Malformed("device tree is outside the platform's reserved \
                                               RAM tail"))));
    }
}
