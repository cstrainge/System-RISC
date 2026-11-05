//! Revisioned inventory of the actual owned System roots. Snapshots retain
//! their rows; atomic PTE reads never alias a live non-atomic table view.
use alloc::{sync::Arc, vec::Vec};
use core::{fmt,
           sync::atomic::{AtomicU64, Ordering}};

use mac_abi::{CORRUPT_DATA, NO_MEMORY, NOT_SUPPORTED, Status, inspection::memory as wire};
use mac_kernel_model::pages::{Access, PhysicalAddress};
use mac_riscv_pages::walk::{ReadTables, Span, Walker};

use super::{BOOT, FrameError, SPACES, Space};
use crate::machine::memory::*;

struct Snapshot
{
    revision: u64,
    rows:     Vec<wire::Region>
}
static REGIONS: spinning_top::Spinlock<Option<Arc<Snapshot>>> = spinning_top::Spinlock::new(None);
pub(crate) struct Regions(Arc<Snapshot>);
impl AsRef<[wire::Region]> for Regions
{
    fn as_ref(&self) -> &[wire::Region]
    {
        &self.0.rows
    }
}
pub(crate) struct Native;
#[derive(Debug)]
pub(crate) enum Error
{
    NotReady,
    Stale,
    Tables(mac_riscv_pages::walk::Error<FrameError>),
    Allocation,
    Layout,
    Pages(crate::machine::pages::Error)
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Stale =>
            {
                f.write_str("System PTEs changed during inventory; obtain a new snapshot and retry")
            }
            Self::NotReady =>
            {
                f.write_str("owned supervisor mappings are not installed; memory inspection is \
                             unavailable")
            }
            Self::Tables(e) => e.fmt(f),
            Self::Allocation =>
            {
                f.write_str("not enough System heap to cache the actual mapping inventory; retry \
                             after releasing memory")
            }
            Self::Layout =>
            {
                f.write_str("actual System mapping inventory conflicts with the checked \
                             heap/stack/guard layout")
            }
            Self::Pages(e) => e.fmt(f)
        }
    }
}
struct Reader<'a>(&'a Space);
impl ReadTables for Reader<'_>
{
    type Error = FrameError;

    fn read_table(&self, physical: PhysicalAddress, output: &mut [u64; 512])
                  -> Result<(), FrameError>
    {
        let cpu = if physical == self.0.owner.root_physical()
        {
            // The root's PageBox is held by AddressSpace, rather than Frames.
            BOOT.get()
                .and_then(|b| b.ram.virtual_address(physical, PAGE_BYTES).ok())
                .ok_or(FrameError::Foreign { physical: physical.0 })?
                .0 as usize
        }
        else
        {
            self.0
                .tables
                .node(physical)
                .map(|node| node.cpu)
                .or_else(|cause| super::modules::table_cpu(physical).ok_or(cause))?
        };
        // SAFETY: the owned permanent root/child PageBox pins the complete table
        // in the permanent physmap. All indices are bounded; no caller address
        // is dereferenced, and A/D bits were installed before publication.
        for (index, word) in output.iter_mut().enumerate()
        {
            *word = unsafe {
                AtomicU64::from_ptr((cpu as *mut u64).add(index)).load(Ordering::Acquire)
            };
        }
        Ok(())
    }
}
fn push(rows: &mut Vec<wire::Region>, row: wire::Region) -> Result<(), Error>
{
    if let Some(last) = rows.last_mut()
    {
        if last.virtual_base.checked_add(last.bytes) == Some(row.virtual_base)
           && last.physical_base.checked_add(last.bytes) == Some(row.physical_base)
           && last.kind == row.kind
           && last.protection == row.protection
        {
            last.bytes = last.bytes.checked_add(row.bytes).ok_or(Error::Layout)?;
            return Ok(());
        }
    }
    rows.try_reserve(1).map_err(|_| Error::Allocation)?;
    rows.push(row);
    Ok(())
}
fn kind(base: u64, span: Span, heap: (u64, u64)) -> u32
{
    if base >= heap.0 && base < heap.1 { wire::SYSTEM_HEAP }
    else if super::system_stack_containing(base).is_some() { wire::SYSTEM_STACK }
    else if (VM_PHYSMAP_START..VM_PHYSMAP_END).contains(&base) { wire::DIRECT_MAP }
    else if (VM_SYSTEM_IMAGE_BASE..VM_SYSTEM_IMAGE_LIMIT).contains(&base) {
        if span.permissions.execute { wire::SYSTEM_CODE }
        else if span.permissions.write { wire::SYSTEM_DATA } else { wire::SYSTEM_READ_ONLY }
    }
    else if (APPLICATION_PRIMARY_BASE..IMAGE_LIMIT).contains(&base) { wire::TRUSTED_APPLICATION_ARENA }
    else if (VM_MODULE_START..VM_MODULE_END).contains(&base) { wire::TRUSTED_MODULE_ARENA }
    else if super::BOOT_MMIO_REGIONS.iter().any(|&(start,end)| (start..end).contains(&base))
        || super::super::power::platform::PLATFORM_MMIO_DEVICES.iter().any(|&(_,_,start,bytes,_,_)| base >= start && base - start < bytes) { wire::DEVICE }
    else { wire::OTHER }
}
fn build_inventory() -> Result<Vec<wire::Region>, Error>
{
    let spaces = SPACES.get().ok_or(Error::NotReady)?;
    let space = &spaces.kernel;
    // Mapping identity comes from the owned root/backing, not
    // the allocator's mutable counters or a former BSS symbol.
    // PTE inventory describes the whole mapped heap, including
    // protected storage. Summary capacity describes only the
    // ordinary allocator; aliases do not add physical owners.
    let (base, bytes) = super::mapped_heap_range().ok_or(Error::NotReady)?;
    let heap = (base as u64, (base as u64).checked_add(bytes as u64).ok_or(Error::Layout)?);
    let heap_bytes = heap.1.checked_sub(heap.0).ok_or(Error::Layout)?;
    if spaces.heap.bytes() < heap_bytes
    {
        return Err(Error::Layout);
    }
    let reader = Reader(space);
    let mut walker = Walker::new(&reader, space.owner.root_physical(), 0).map_err(Error::Tables)?;
    let mut rows = Vec::new();
    while let Some(span) = walker.next_span().map_err(Error::Tables)?
    {
        let end = span.virtual_base.checked_add(span.bytes).ok_or(Error::Layout)?;
        let mut base = span.virtual_base;
        while base < end
        {
            let stop =
                [heap.0, heap.1].into_iter().filter(|&v| v > base && v < end).min().unwrap_or(end);
            let p = span.permissions;
            if base >= heap.0
               && base < heap.1
               && (span.physical_base.0 + base - span.virtual_base
                   != spaces.heap.physical().0 + base - heap.0
                   || !p.read
                   || !p.write
                   || p.execute
                   || !p.global
                   || p.access != Access::Supervisor)
            {
                return Err(Error::Layout);
            }
            if let Some(stack) = super::system_stack_containing(base)
            {
                let top = stack.top() as u64;
                let bottom = top.checked_sub(stack.bytes()).ok_or(Error::Layout)?;
                if stop > top
                   || span.physical_base.0 + base - span.virtual_base
                      != stack.physical().0 + base - bottom
                   || !p.read
                   || !p.write
                   || p.execute
                   || !p.global
                   || p.access != Access::Supervisor
                {
                    return Err(Error::Layout);
                }
            }
            let protection = u32::from(p.read) * wire::READABLE
                             | u32::from(p.write) * wire::WRITABLE
                             | u32::from(p.execute) * wire::EXECUTABLE
                             | u32::from(p.global) * wire::GLOBAL
                             | u32::from(p.access == Access::User) * wire::USER;
            push(&mut rows,
                 wire::Region { virtual_base: base,
                                physical_base: span.physical_base.0 + base - span.virtual_base,
                                bytes: stop - base,
                                protection,
                                kind: kind(base, span, heap) })?;
            base = stop;
        }
    }
    for stack in spaces.stacks.as_slice()
    {
        let top = stack.top() as u64;
        let lower = top.checked_sub(stack.bytes())
                       .and_then(|bottom| bottom.checked_sub(PAGE_BYTES))
                       .ok_or(Error::Layout)?;
        for base in [lower, top]
        {
            let end = base.checked_add(PAGE_BYTES).ok_or(Error::Layout)?;
            if rows.iter().any(|r| r.virtual_base < end && r.virtual_base + r.bytes > base)
            {
                return Err(Error::Layout);
            }
            push(&mut rows,
                 wire::Region { virtual_base: base,
                                bytes: PAGE_BYTES,
                                kind: wire::GUARD,
                                ..Default::default() })?;
        }
    }
    rows.sort_unstable_by_key(|r| r.virtual_base);
    Ok(rows)
}
fn inventory() -> Result<Arc<Snapshot>, Error>
{
    let revision = super::modules::revision().ok_or(Error::Stale)?;
    if let Some(snapshot) = super::interrupts::with_masked(|| {
        REGIONS.lock().as_ref().filter(|snapshot| snapshot.revision == revision).cloned()
    })
    {
        return Ok(snapshot);
    }
    crate::context::without_recovery(|| {
        crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
            let rows = build_inventory()?;
            if super::modules::revision() != Some(revision)
            {
                return Err(Error::Stale);
            }
            let snapshot = Arc::try_new(Snapshot { revision,
                                                   rows }).map_err(|_| Error::Allocation)?;
            let old = super::interrupts::with_masked(|| REGIONS.lock().replace(snapshot.clone()));
            drop(old);
            Ok(snapshot)
        })
    })
}

impl crate::toolbox::inspection::memory::Source for Native
{
    type Error = Error;
    type Rows = Regions;

    fn status(error: &Error) -> Status
    {
        match error
        {
            Error::NotReady => NOT_SUPPORTED,
            Error::Stale => mac_abi::SCAN_STALE,
            Error::Allocation => NO_MEMORY,
            Error::Tables(_) | Error::Layout | Error::Pages(_) => CORRUPT_DATA
        }
    }

    fn revision(&self) -> Result<u64, Error>
    {
        SPACES.get().ok_or(Error::NotReady)?;
        super::modules::revision().ok_or(Error::Stale)
    }

    fn regions(&self) -> Result<Regions, Error>
    {
        inventory().map(Regions)
    }

    fn summary(&self) -> Result<wire::Summary, Error>
    {
        let regions = inventory()?;
        let boot = BOOT.get().ok_or(Error::NotReady)?;
        let root = SPACES.get().ok_or(Error::NotReady)?.kernel.owner.root_physical().0;
        let ram_bytes = boot.memory
                            .banks()
                            .iter()
                            .try_fold(0u64, |n, b| n.checked_add(b.length))
                            .ok_or(Error::Layout)?;
        let (_, used, capacity) = crate::kernel::heap_statistics();
        let attribution = crate::allocator::statistics();
        Ok(wire::Summary { revision: regions.revision,
                           page_bytes: PAGE_BYTES,
                           ram_bytes,
                           free_pages: crate::machine::pages::free_pages().map_err(Error::Pages)?,
                           heap_used: used,
                           heap_capacity: capacity,
                           root_physical: root,
                           region_count: regions.rows.len() as u64,
                           ordinary_foundation_heap_bytes: attribution.foundation,
                           tracked_foundation_physical_bytes: attribution.foundation_physical,
                           tracked_ordinary_system_physical_bytes: attribution.ordinary_physical,
                           flags: wire::SHARED_SUPERVISOR | wire::ACCOUNTING_UNAVAILABLE,
                           reserved: 0 })
    }
}
