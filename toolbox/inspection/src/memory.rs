//! Read-only System mapping snapshots. The map revision does not claim that
//! heap/page-pool counters were sampled simultaneously across the machine.
use mac_abi::inspection::memory as wire;
use mac_traps::{Client, Error, types::InlineText};

use super::{Failure, Inspection, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionKind
{
    Device,
    TrustedApplicationArena,
    TrustedModuleArena,
    SystemCode,
    SystemReadOnly,
    SystemData,
    SystemHeap,
    SystemStack,
    Guard,
    DirectMap,
    Other
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Protection
{
    pub read:    bool,
    pub write:   bool,
    pub execute: bool,
    pub user:    bool,
    pub global:  bool
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region
{
    pub virtual_base:  u64,
    /// None for an unmapped guard. Physical address zero is a valid mapping.
    pub physical_base: Option<u64>,
    pub bytes:         u64,
    pub protection:    Protection,
    pub kind:          RegionKind
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemorySummary
{
    pub revision:                          u64,
    pub page_bytes:                        u64,
    pub ram_bytes:                         u64,
    pub free_pages:                        u64,
    pub heap_used:                         u64,
    pub heap_capacity:                     u64,
    pub root_physical:                     u64,
    pub region_count:                      u64,
    /// Foundation occupancy within the ordinary heap, not additional memory.
    pub ordinary_foundation_heap_bytes:    u64,
    /// Rounded physical leases for the covered CPU/service storage only.
    /// Excludes old page tables, pool metadata, ROM reservations and protected
    /// backing; mapping aliases are not charges. Whole-System accounting is
    /// still unavailable when accounting_unavailable is true.
    pub tracked_foundation_physical_bytes: u64,
    /// Applications still share this supervisor address space. Full per-domain
    /// heap/accounting, recovery reserves and private-app gauges are unavailable;
    /// domain rows separately expose their explicitly covered execution backing.
    pub shared_supervisor:                 bool,
    pub accounting_unavailable:            bool
}
fn failure(error: Error, call: &wire::Call) -> Failure
{
    Failure { cause:  error,
              detail: InlineText::from_reply(call.detail, call.detail_length).unwrap_or_default() }
}
fn invalid(message: &str) -> Failure
{
    let mut bytes = [0; mac_abi::inspection::DETAIL_BYTES];
    let count = message.len().min(bytes.len());
    bytes[..count].copy_from_slice(&message.as_bytes()[..count]);
    Failure { cause:  Error::BadAbi,
              detail: InlineText::from_reply(bytes, count as u32).unwrap_or_default() }
}
#[derive(Clone, Copy)]
pub struct SystemMemory<'a>
{
    client:  Client<'a>,
    summary: MemorySummary
}
impl<'a> Inspection<'a>
{
    /// Requires the System inspection grant: addresses are cross-domain detail,
    /// unlike the public existence/enumeration of kind and handler registries.
    pub fn system_memory(self) -> Result<SystemMemory<'a>>
    {
        let mut call = wire::Call::default();
        self.client.inspect_memory(&mut call).map_err(|e| failure(e, &call))?;
        let s = call.summary;
        if s.reserved != 0
           || s.revision == 0
           || s.revision != call.revision
           || s.flags & !(wire::SHARED_SUPERVISOR | wire::ACCOUNTING_UNAVAILABLE) != 0
           || !s.page_bytes.is_power_of_two()
           || s.root_physical % s.page_bytes != 0
           || s.heap_used > s.heap_capacity
           || s.ram_bytes / s.page_bytes < s.free_pages
        {
            return Err(invalid("invalid System memory summary from resident service; rebuild \
                                System and application together"));
        }
        Ok(SystemMemory { client:  self.client,
                          summary: MemorySummary { revision:                          s.revision,
                                                   page_bytes:                        s.page_bytes,
                                                   ram_bytes:                         s.ram_bytes,
                                                   free_pages:                        s.free_pages,
                                                   heap_used:                         s.heap_used,
                                                   heap_capacity:
                                                       s.heap_capacity,
                                                   root_physical:
                                                       s.root_physical,
                                                   region_count:
                                                       s.region_count,
                                                   ordinary_foundation_heap_bytes:
                                                       s.ordinary_foundation_heap_bytes,
                                                   tracked_foundation_physical_bytes:
                                                       s.tracked_foundation_physical_bytes,
                                                   shared_supervisor:
                                                       s.flags & wire::SHARED_SUPERVISOR != 0,
                                                   accounting_unavailable:
                                                       s.flags & wire::ACCOUNTING_UNAVAILABLE != 0 } })
    }
}
#[derive(Clone, Copy, Debug)]
pub struct RegionBatch
{
    start: u64,
    rows:  [wire::Region; wire::BATCH],
    count: usize
}
impl RegionBatch
{
    pub fn start(&self) -> u64
    {
        self.start
    }

    pub fn len(&self) -> usize
    {
        self.count
    }

    pub fn is_empty(&self) -> bool
    {
        self.count == 0
    }

    pub fn at(&self, index: u64) -> Option<Region>
    {
        let index = usize::try_from(index.checked_sub(self.start)?).ok()?;
        (index < self.count).then(|| decode(self.rows[index])).flatten()
    }
}
fn decode(r: wire::Region) -> Option<Region>
{
    let kind = match r.kind
    {
        wire::DEVICE => RegionKind::Device,
        wire::TRUSTED_APPLICATION_ARENA => RegionKind::TrustedApplicationArena,
        wire::TRUSTED_MODULE_ARENA => RegionKind::TrustedModuleArena,
        wire::SYSTEM_CODE => RegionKind::SystemCode,
        wire::SYSTEM_READ_ONLY => RegionKind::SystemReadOnly,
        wire::SYSTEM_DATA => RegionKind::SystemData,
        wire::SYSTEM_HEAP => RegionKind::SystemHeap,
        wire::SYSTEM_STACK => RegionKind::SystemStack,
        wire::GUARD => RegionKind::Guard,
        wire::DIRECT_MAP => RegionKind::DirectMap,
        wire::OTHER => RegionKind::Other,
        _ => return None
    };
    if r.bytes == 0
       || r.virtual_base.checked_add(r.bytes).is_none()
       || r.protection
          & !(wire::READABLE | wire::WRITABLE | wire::EXECUTABLE | wire::USER | wire::GLOBAL)
          != 0
       || r.protection & wire::WRITABLE != 0 && r.protection & wire::READABLE == 0
    {
        return None;
    }
    let physical_base = if kind == RegionKind::Guard
    {
        if r.protection != 0 || r.physical_base != 0
        {
            return None;
        }
        None
    }
    else
    {
        if r.protection & (wire::READABLE | wire::EXECUTABLE) == 0
           || r.physical_base.checked_add(r.bytes).is_none()
        {
            return None;
        }
        Some(r.physical_base)
    };
    Some(Region { virtual_base: r.virtual_base,
                  physical_base,
                  bytes: r.bytes,
                  kind,
                  protection: Protection { read:    r.protection & wire::READABLE != 0,
                                           write:   r.protection & wire::WRITABLE != 0,
                                           execute: r.protection & wire::EXECUTABLE != 0,
                                           user:    r.protection & wire::USER != 0,
                                           global:  r.protection & wire::GLOBAL != 0 } })
}
impl<'a> SystemMemory<'a>
{
    pub fn summary(self) -> MemorySummary
    {
        self.summary
    }

    /// Fetches a bounded transport batch, not a new resident snapshot object.
    /// Rechecks caller authority and mapping revision on every read.
    pub fn batch(self, index: u64) -> Result<RegionBatch>
    {
        let mut call = wire::Call { operation: wire::READ,
                                    revision: self.summary.revision,
                                    index,
                                    ..Default::default() };
        self.client.inspect_memory(&mut call).map_err(|e| failure(e, &call))?;
        let count = call.count as usize;
        if call.revision != self.summary.revision
           || count == 0
           || count > wire::BATCH
           || index >= self.summary.region_count
           || call.next != index.saturating_add(count as u64)
           || call.next > self.summary.region_count
           || call.flags != if call.next < self.summary.region_count { wire::HAS_NEXT } else { 0 }
        {
            return Err(invalid("invalid System mapping batch range; rebuild System and \
                                application together"));
        }
        let mut previous = None;
        for r in &call.regions[..count]
        {
            if decode(*r).is_none()
               || previous.is_some_and(|end| r.virtual_base < end)
               || r.virtual_base % self.summary.page_bytes != 0
               || r.bytes % self.summary.page_bytes != 0
               || r.physical_base % self.summary.page_bytes != 0
            {
                return Err(invalid("invalid or overlapping System mapping row; rebuild System \
                                    and application together"));
            }
            previous = r.virtual_base.checked_add(r.bytes);
        }
        Ok(RegionBatch { start: index,
                         rows: call.regions,
                         count })
    }

    pub fn regions(self) -> Regions<'a>
    {
        Regions { memory: self,
                  index:  0,
                  batch:  None,
                  failed: false }
    }
}
pub struct Regions<'a>
{
    memory: SystemMemory<'a>,
    index:  u64,
    batch:  Option<RegionBatch>,
    failed: bool
}
impl Iterator for Regions<'_>
{
    type Item = Result<Region>;

    fn next(&mut self) -> Option<Self::Item>
    {
        if self.failed || self.index >= self.memory.summary.region_count
        {
            return None;
        }
        if let Some(row) = self.batch.as_ref().and_then(|b| b.at(self.index))
        {
            self.index += 1;
            return Some(Ok(row));
        }
        match self.memory.batch(self.index)
        {
            Ok(batch) =>
            {
                self.batch = Some(batch);
                self.next()
            }
            Err(error) =>
            {
                self.failed = true;
                Some(Err(error))
            }
        }
    }
}
impl core::iter::FusedIterator for Regions<'_>
{
}
