//! Actual, permanently owned System PTE inventory. Immutable roots permit one
//! lazy metadata cache; no boot plan or second registry is used as evidence.
use alloc::vec::Vec;
use core::{fmt, ptr};

use mac_abi::{CORRUPT_DATA, NO_MEMORY, NOT_SUPPORTED, Status, inspection::memory as wire};
use mac_kernel_model::pages::{Access, PhysicalAddress};
use mac_riscv_pages::walk::{ReadTables, Span, Walker};

use super::{super::memory::*, BOOT, FrameError, SPACES, Space};

static REGIONS: spin::Once<Vec<wire::Region>> = spin::Once::new();
pub(crate) struct Native;
#[derive(Debug)]
pub(crate) enum Error
{
    NotReady,
    Tables(mac_riscv_pages::walk::Error<FrameError>),
    Allocation,
    Layout,
    Pages(super::super::pages::Error)
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
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
            self.0.tables.node(physical)?.cpu
        };
        // SAFETY: the owned immutable root/child PageBox pins the complete table
        // in the permanent physmap. All indices are bounded; no caller address
        // is dereferenced, and A/D bits were installed before publication.
        for (index, word) in output.iter_mut().enumerate()
        {
            *word = unsafe { ptr::read_volatile((cpu as *const u64).add(index)) };
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
    else if (VM_SYSTEM_STACKS_START + PAGE_BYTES..VM_SYSTEM_STACKS_START + PAGE_BYTES + SYSTEM_STACK_BYTES).contains(&base) { wire::SYSTEM_STACK }
    else if (VM_PHYSMAP_START..VM_PHYSMAP_END).contains(&base) { wire::DIRECT_MAP }
    else if (VM_SYSTEM_IMAGE_BASE..VM_SYSTEM_IMAGE_LIMIT).contains(&base) {
        if span.permissions.execute { wire::SYSTEM_CODE }
        else if span.permissions.write { wire::SYSTEM_DATA } else { wire::SYSTEM_READ_ONLY }
    }
    else if (APPLICATION_PRIMARY_BASE..IMAGE_LIMIT).contains(&base) { wire::TRUSTED_APPLICATION_ARENA }
    else if (MODULE_BASE..MODULE_LIMIT).contains(&base) { wire::TRUSTED_MODULE_ARENA }
    else if super::BOOT_MMIO_REGIONS.iter().any(|&(start,end)| (start..end).contains(&base))
        || super::super::power::platform::PLATFORM_MMIO_DEVICES.iter().any(|&(_,_,start,bytes,_,_)| base >= start && base - start < bytes) { wire::DEVICE }
    else { wire::OTHER }
}
fn inventory() -> Result<&'static [wire::Region], Error>
{
    // Prevent re-entry/preemption of the lazy initializer. Work is proportional
    // to actual table pages, not base pages in RAM, and occurs on explicit query
    // only. Failure leaves Once empty, so exhaustion is retryable.
    super::interrupts::with_masked(|| {
        REGIONS.try_call_once(|| {
                   let spaces = SPACES.get().ok_or(Error::NotReady)?;
                   let space = &spaces.kernel;
                   // Mapping identity comes from the owned root/backing, not
                   // the allocator's mutable counters or a former BSS symbol.
                   let heap = (VM_SYSTEM_HEAP_BASE, VM_SYSTEM_HEAP_LIMIT);
                   let heap_bytes = heap.1.checked_sub(heap.0).ok_or(Error::Layout)?;
                   if spaces.heap.bytes() < heap_bytes
                   {
                       return Err(Error::Layout);
                   }
                   let reader = Reader(space);
                   let mut walker =
                       Walker::new(&reader, space.owner.root_physical(), 0).map_err(Error::Tables)?;
                   let mut rows = Vec::new();
                   while let Some(span) = walker.next_span().map_err(Error::Tables)?
                   {
                       let end = span.virtual_base.checked_add(span.bytes).ok_or(Error::Layout)?;
                       let mut base = span.virtual_base;
                       while base < end
                       {
                           let stop = [heap.0, heap.1].into_iter()
                                                      .filter(|&v| v > base && v < end)
                                                      .min()
                                                      .unwrap_or(end);
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
                           let protection = u32::from(p.read) * wire::READABLE
                                            | u32::from(p.write) * wire::WRITABLE
                                            | u32::from(p.execute) * wire::EXECUTABLE
                                            | u32::from(p.global) * wire::GLOBAL
                                            | u32::from(p.access == Access::User) * wire::USER;
                           push(&mut rows,
                                wire::Region { virtual_base: base,
                                               physical_base: span.physical_base.0 + base
                                                              - span.virtual_base,
                                               bytes: stop - base,
                                               protection,
                                               kind: kind(base, span, heap) })?;
                           base = stop;
                       }
                   }
                   for base in [VM_SYSTEM_STACKS_START,
                                VM_SYSTEM_STACKS_START + PAGE_BYTES + SYSTEM_STACK_BYTES]
                   {
                       let end = base.checked_add(PAGE_BYTES).ok_or(Error::Layout)?;
                       if rows.iter()
                              .any(|r| r.virtual_base < end && r.virtual_base + r.bytes > base)
                       {
                           return Err(Error::Layout);
                       }
                       push(&mut rows,
                            wire::Region { virtual_base: base,
                                           bytes: PAGE_BYTES,
                                           kind: wire::GUARD,
                                           ..Default::default() })?;
                   }
                   rows.sort_unstable_by_key(|r| r.virtual_base);
                   Ok(rows)
               })
    }).map(Vec::as_slice)
}
impl crate::toolbox::inspection::memory::Source for Native
{
    type Error = Error;

    fn status(error: &Error) -> Status
    {
        match error
        {
            Error::NotReady => NOT_SUPPORTED,
            Error::Allocation => NO_MEMORY,
            Error::Tables(_) | Error::Layout | Error::Pages(_) => CORRUPT_DATA
        }
    }

    fn revision(&self) -> Result<u64, Error>
    {
        SPACES.get().ok_or(Error::NotReady)?;
        Ok(1) // These permanently owned roots are immutable until reset.
    }

    fn regions(&self) -> Result<&[wire::Region], Error>
    {
        inventory()
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
        Ok(wire::Summary { revision: self.revision()?,
                           page_bytes: PAGE_BYTES,
                           ram_bytes,
                           free_pages: super::super::pages::free_pages().map_err(Error::Pages)?,
                           heap_used: used,
                           heap_capacity: capacity,
                           root_physical: root,
                           region_count: regions.len() as u64,
                           flags: wire::SHARED_SUPERVISOR | wire::ACCOUNTING_UNAVAILABLE,
                           reserved: 0 })
    }
}
