//! Bootstrap reservation account. Build policy and discovered extents stay distinct.
#![forbid(unsafe_code)]
use alloc::vec::Vec;
use core::{fmt, ops::Range};

use mac_abi::BootMemoryRange;
use mac_devicetree::Tree;

use crate::devicetree::Error;

include!(concat!(env!("OUT_DIR"), "/platform-memory.rs"));

pub struct Memory
{
    pub ram:      Vec<BootMemoryRange>,
    pub reserved: Vec<BootMemoryRange>
}
fn primary(tree: &Tree<'_>) -> Result<Range<u64>, Error>
{
    let mut primary = None;
    let mut duplicate = false;
    // The shared reader decodes `reg` against the root's declared cells and
    // refuses a bank it cannot decode, so a malformed tree is a refusal here
    // rather than a bank read out of the wrong width.
    tree.memory_checked(|base, bytes| {
            if base == RAM_BASE
            {
                if primary.replace(base..base + bytes).is_some()
                {
                    duplicate = true;
                }
            }
        })
        .map_err(|_| {
            Error::Malformed("device tree RAM bank is not decodable as declared")
        })?;
    if duplicate
    {
        return Err(Error::Malformed("duplicate primary RAM bank in device tree"));
    }
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
    // The shared reader validates the container's cell declaration, each CPU's
    // device_type and reg width, and the status of disabled CPUs as well as
    // enabled ones; only enabled identities reach this vector.
    let mut cpus: Vec<mac_abi::boot::topology::Cpu> = Vec::new();
    let mut failure: Option<Error> = None;
    tree.cpus_checked(|hart| {
            if failure.is_some()
            {
                return;
            }
            if cpus.try_reserve(1).is_err()
            {
                failure = Some(Error::AllocationFailed("allocation failed for enabled CPU \
                                                        metadata"));
                return;
            }
            cpus.push(mac_abi::boot::topology::Cpu { hart });
        })
        .map_err(|_| Error::Malformed("device tree CPU nodes are not admissible"))?;
    if let Some(error) = failure
    {
        return Err(error.into());
    }
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
        let mut failure: Option<Error> = None;
        tree.memory_checked(|base, bytes| {
                if failure.is_some()
                {
                    return;
                }
                if !base.is_multiple_of(PAGE_BYTES) || !bytes.is_multiple_of(PAGE_BYTES)
                {
                    failure = Some(Error::Malformed("device tree RAM bank is not page aligned"));
                    return;
                }
                if ram.try_reserve(1).is_err()
                {
                    failure = Some(Error::AllocationFailed("allocation failed for discovered RAM \
                                                            metadata"));
                    return;
                }
                ram.push(wire(base..base + bytes));
            })
            .map_err(|_| {
                Error::Malformed("device tree RAM bank is not decodable as declared")
            })?;
        if let Some(error) = failure
        {
            return Err(error);
        }
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
        // The reader reports the reservation block and the static
        // `/reserved-memory` children, and refuses a child that asks for dynamic
        // allocation rather than leaving those bytes claimable.
        let mut failure: Option<Error> = None;
        tree.reservations_checked(|range| {
                if failure.is_some()
                {
                    return Ok(());
                }
                let start = range.start & !(PAGE_BYTES - 1);
                let Some(padded) = range.end.checked_add(PAGE_BYTES - 1)
                else
                {
                    failure = Some(Error::Malformed("device tree reservation alignment \
                                                     overflows"));
                    return Ok(());
                };
                if let Err(error) = memory.reserve_union(start..padded & !(PAGE_BYTES - 1))
                {
                    failure = Some(error);
                }
                Ok(())
            })
            .map_err(|_| Error::Malformed("device tree reservation is not decodable"))?;
        if let Some(error) = failure
        {
            return Err(error);
        }
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

    /// A minimal valid blob: an unnamed root declaring two address and two size
    /// cells, one `memory@<base>` node per bank, and a terminating reservation
    /// entry. This lives here rather than in the reader because the reader is a
    /// separate crate and cannot lend test scaffolding across the boundary.
    fn blob(banks: &[(u64, u64)], reserved: bool) -> Vec<u8>
    {
        const MAGIC: u32 = 0xd00d_feed;
        let mut structure: Vec<u8> = Vec::new();
        let mut strings: Vec<u8> = Vec::new();
        let mut intern = |strings: &mut Vec<u8>, name: &str| -> u32 {
            let offset = strings.len() as u32;
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
            offset
        };
        let mut begin = |structure: &mut Vec<u8>, name: &str| {
            structure.extend_from_slice(&1u32.to_be_bytes());
            structure.extend_from_slice(name.as_bytes());
            structure.push(0);
            while structure.len() % 4 != 0
            {
                structure.push(0);
            }
        };
        let mut property = |structure: &mut Vec<u8>, strings: &mut Vec<u8>, name: &str,
                            value: &[u8]| {
            let offset = intern(strings, name);
            structure.extend_from_slice(&3u32.to_be_bytes());
            structure.extend_from_slice(&(value.len() as u32).to_be_bytes());
            structure.extend_from_slice(&offset.to_be_bytes());
            structure.extend_from_slice(value);
            while structure.len() % 4 != 0
            {
                structure.push(0);
            }
        };
        begin(&mut structure, "");
        property(&mut structure, &mut strings, "#address-cells", &2u32.to_be_bytes());
        property(&mut structure, &mut strings, "#size-cells", &2u32.to_be_bytes());
        for (base, size) in banks
        {
            begin(&mut structure, &alloc::format!("memory@{base:x}"));
            property(&mut structure, &mut strings, "device_type", b"memory\0");
            let mut reg = Vec::new();
            // Two address cells and two size cells, so a bank above 4 GiB keeps
            // its high word instead of being truncated to nothing.
            reg.extend_from_slice(&((*base >> 32) as u32).to_be_bytes());
            reg.extend_from_slice(&(*base as u32).to_be_bytes());
            reg.extend_from_slice(&((*size >> 32) as u32).to_be_bytes());
            reg.extend_from_slice(&(*size as u32).to_be_bytes());
            property(&mut structure, &mut strings, "reg", &reg);
            structure.extend_from_slice(&2u32.to_be_bytes());
        }
        structure.extend_from_slice(&2u32.to_be_bytes());
        structure.extend_from_slice(&9u32.to_be_bytes());
        let reserved_block = 16usize;
        let structure_offset = 40 + reserved_block;
        let strings_offset = structure_offset + structure.len();
        let total = strings_offset + strings.len();
        let mut blob = Vec::new();
        blob.extend_from_slice(&MAGIC.to_be_bytes());
        blob.extend_from_slice(&(total as u32).to_be_bytes());
        blob.extend_from_slice(&(structure_offset as u32).to_be_bytes());
        blob.extend_from_slice(&(strings_offset as u32).to_be_bytes());
        blob.extend_from_slice(&40u32.to_be_bytes());
        blob.extend_from_slice(&17u32.to_be_bytes());
        blob.extend_from_slice(&16u32.to_be_bytes());
        blob.extend_from_slice(&0u32.to_be_bytes());
        blob.extend_from_slice(&(strings.len() as u32).to_be_bytes());
        blob.extend_from_slice(&(structure.len() as u32).to_be_bytes());
        if reserved
        {
            blob.extend_from_slice(&(RAM_BASE).to_be_bytes());
            blob.extend_from_slice(&(PAGE_BYTES).to_be_bytes());
            blob.extend_from_slice(&0u64.to_be_bytes());
            blob.extend_from_slice(&0u64.to_be_bytes());
        }
        else
        {
            blob.extend_from_slice(&[0u8; 16]);
        }
        blob.extend_from_slice(&structure);
        blob.extend_from_slice(&strings);
        blob
    }

    #[test]
    fn real_policy_preserves_discovered_size_and_disjoint_reservations()
    {
        let bytes = blob(&[(RAM_BASE, 0x200000000)], false);
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
        let bytes = blob(&[(RAM_BASE, MINIMUM_RAM_BYTES / 2)], false);
        let tree = Tree::new(&bytes).unwrap();
        assert_eq!(admit(&tree),
                   Err(Error::Malformed("discovered RAM is smaller than the platform minimum; \
                                         increase VM RAM or select another platform")));
        let bytes = blob(&[(RAM_BASE, MINIMUM_RAM_BYTES)], false);
        let tree = Tree::new(&bytes).unwrap();
        assert!(matches!(Memory::discover(&tree, IMAGE_BASE),
                         Err(Error::Malformed("device tree is outside the platform's reserved \
                                               RAM tail"))));
    }
}
