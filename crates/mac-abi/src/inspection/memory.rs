//! CPU-addressed mapping inspection, a separate payload on INSPECTION.
//! Snapshot row positions are data, never table pointers or object IDs.
pub const BEGIN: u32 = 7;
pub const READ: u32 = 8;
pub const SYSTEM: u32 = 0;
pub const SHARED_SUPERVISOR: u32 = 1;
pub const ACCOUNTING_UNAVAILABLE: u32 = 2;
pub const HAS_NEXT: u32 = 4;
pub const READABLE: u32 = 1;
pub const WRITABLE: u32 = 2;
pub const EXECUTABLE: u32 = 4;
pub const USER: u32 = 8;
pub const GLOBAL: u32 = 16;
pub const DEVICE: u32 = 0;
pub const TRUSTED_APPLICATION_ARENA: u32 = 1;
pub const TRUSTED_MODULE_ARENA: u32 = 2;
pub const SYSTEM_CODE: u32 = 3;
pub const SYSTEM_READ_ONLY: u32 = 4;
pub const SYSTEM_DATA: u32 = 5;
pub const SYSTEM_HEAP: u32 = 6;
pub const SYSTEM_STACK: u32 = 7;
pub const GUARD: u32 = 8;
pub const DIRECT_MAP: u32 = 9;
pub const OTHER: u32 = 10;
/// A transport batch size, not a bound on mapping count or application capacity.
pub const BATCH: usize = 16;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Region
{
    pub virtual_base:  u64,
    pub physical_base: u64,
    pub bytes:         u64,
    pub protection:    u32,
    pub kind:          u32
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Summary
{
    pub revision:                          u64,
    pub page_bytes:                        u64,
    pub ram_bytes:                         u64,
    pub free_pages:                        u64,
    pub heap_used:                         u64,
    pub heap_capacity:                     u64,
    pub root_physical:                     u64,
    pub region_count:                      u64,
    /// Foundation attribution within the ordinary heap, never extra capacity.
    pub ordinary_foundation_heap_bytes:    u64,
    /// Selected CPU/service backing owned by unique physical receipts. Partial
    /// coverage excludes old page tables, pool metadata, ROM reservations and
    /// the protected floor. Not extra heap use or a sum of mapping aliases.
    pub tracked_foundation_physical_bytes: u64,
    pub flags:                             u32,
    pub reserved:                          u32
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Call
{
    pub operation:     u32,
    pub source:        u32,
    pub revision:      u64,
    pub index:         u64,
    pub next:          u64,
    pub count:         u32,
    pub flags:         u32,
    pub summary:       Summary,
    pub regions:       [Region; BATCH],
    pub detail:        [u8; super::DETAIL_BYTES],
    pub detail_length: u32,
    pub reserved:      u32
}
impl Default for Call
{
    fn default() -> Self
    {
        Self { operation:     BEGIN,
               source:        SYSTEM,
               revision:      0,
               index:         0,
               next:          0,
               count:         0,
               flags:         0,
               summary:       Summary::default(),
               regions:       [Region::default(); BATCH],
               detail:        [0; super::DETAIL_BYTES],
               detail_length: 0,
               reserved:      0 }
    }
}
