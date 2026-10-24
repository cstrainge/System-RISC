//! Native, read-only lifetime inspection. No pointers or caller identity on wire.
pub const BEGIN: u32 = 0;
pub const DOMAIN: u32 = 1;
pub const LEDGER_POOL: u32 = 2;
pub const KIND_BEGIN: u32 = 3;
pub const KIND: u32 = 4;
pub const KIND_ID: u32 = 5;
pub const OWNER_SYSTEM: u32 = 0;
pub const OWNER_PROCESS: u32 = 1;
pub const OWNER_MODULE: u32 = 2;
pub const SOURCE_RESIDENT: u32 = 0;
pub const SOURCE_BUNDLE_RESOURCE: u32 = 1;
pub const CAPACITY_GROWABLE: u32 = 0;
pub const CAPACITY_QUOTA: u32 = 1;
pub const CAPACITY_BOUND: u32 = 2;
pub const SELF: u32 = 0;
pub const ALL: u32 = 1;
pub const INSPECT_DOMAINS: u64 = 1;
/// Coalesced advisory wake; obtain a new membership snapshot after delivery.
pub const CHANGED_EVENT: u32 = 0x0d00_0000;
pub const PROCESS: u32 = 1;
/// Coverage is deliberately explicit while resident managers migrate.
pub const PROCESS_DOMAINS: u64 = 1;
pub const PARTIAL_LEDGERS: u64 = 2;
pub const ACCEPTING: u32 = 1;
pub const FOREGROUND: u32 = 2;
pub const COUNTERS_SATURATED: u32 = 4;
pub const DETAIL_BYTES: usize = 192;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RegisteredKind
{
    pub id:              u64,
    pub owner:           u64,
    pub owner_kind:      u32,
    pub namespace:       [u8; 4],
    pub policy:          u32,
    pub admitting:       u32,
    pub limit:           u64,
    pub uses:            u64,
    pub label:           [u8; crate::INLINE_STRING_BYTES],
    pub label_length:    u32,
    pub provider:        [u8; crate::INLINE_STRING_BYTES],
    pub provider_length: u32,
    pub source:          u32,
    pub bundle:          [u8; 16],
    pub resource_kind:   [u8; 4],
    pub resource_id:     i16,
    pub reserved:        u16,
    pub reason:          [u8; DETAIL_BYTES],
    pub reason_length:   u32
}
impl Default for RegisteredKind
{
    fn default() -> Self
    {
        Self { id:              0,
               owner:           0,
               owner_kind:      0,
               namespace:       [0; 4],
               policy:          0,
               admitting:       0,
               limit:           0,
               uses:            0,
               label:           [0; crate::INLINE_STRING_BYTES],
               label_length:    0,
               provider:        [0; crate::INLINE_STRING_BYTES],
               provider_length: 0,
               source:          0,
               bundle:          [0; 16],
               resource_kind:   [0; 4],
               resource_id:     0,
               reserved:        0,
               reason:          [0; DETAIL_BYTES],
               reason_length:   0 }
    }
}

/// Prepared owner-pool census. Registration/release-dispatch metadata is a
/// distinct query; an empty pool does not imply a dead kind registration.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Pool
{
    pub registration:        u64,
    pub kind:                [u8; 4],
    pub policy:              u32,
    pub limit:               u64,
    pub live:                u64,
    pub retired:             u64,
    pub pending:             u64,
    pub quarantined:         u64,
    pub slots:               u64,
    pub births:              u64,
    pub deaths:              u64,
    pub generation_headroom: u64,
    pub reason:              [u8; DETAIL_BYTES],
    pub reason_length:       u32,
    pub flags:               u32
}
impl Default for Pool
{
    fn default() -> Self
    {
        Self { registration:        0,
               kind:                [0; 4],
               policy:              CAPACITY_GROWABLE,
               limit:               0,
               live:                0,
               retired:             0,
               pending:             0,
               quarantined:         0,
               slots:               0,
               births:              0,
               deaths:              0,
               generation_headroom: 0,
               reason:              [0; DETAIL_BYTES],
               reason_length:       0,
               flags:               0 }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Domain
{
    pub incarnation:     u64,
    pub kind:            u32,
    pub flags:           u32,
    pub ledger_revision: u64,
    pub kinds:           u64,
    pub live:            u64,
    pub retired:         u64,
    pub pending:         u64,
    pub quarantined:     u64,
    pub slots:           u64,
    pub births:          u64,
    pub deaths:          u64,
    pub name:            [u8; crate::INLINE_STRING_BYTES],
    pub name_length:     u32,
    pub reserved:        u32
}
impl Default for Domain
{
    fn default() -> Self
    {
        Self { incarnation:     0,
               kind:            0,
               flags:           0,
               ledger_revision: 0,
               kinds:           0,
               live:            0,
               retired:         0,
               pending:         0,
               quarantined:     0,
               slots:           0,
               births:          0,
               deaths:          0,
               name:            [0; crate::INLINE_STRING_BYTES],
               name_length:     0,
               reserved:        0 }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Call
{
    pub operation:       u32,
    pub scope:           u32,
    pub revision:        u64,
    pub index:           u64,
    pub count:           u64,
    pub coverage:        u64,
    pub ledger_revision: u64,
    pub subject:         u64,
    pub pool_index:      u64,
    pub pool:            Pool,
    pub kind:            RegisteredKind,
    pub domain:          Domain,
    pub detail:          [u8; DETAIL_BYTES],
    pub detail_length:   u32,
    pub reserved:        u32
}
impl Default for Call
{
    fn default() -> Self
    {
        Self { operation:       BEGIN,
               scope:           SELF,
               revision:        0,
               index:           0,
               count:           0,
               coverage:        0,
               ledger_revision: 0,
               subject:         0,
               pool_index:      0,
               pool:            Pool::default(),
               kind:            RegisteredKind::default(),
               domain:          Domain::default(),
               detail:          [0; DETAIL_BYTES],
               detail_length:   0,
               reserved:        0 }
    }
}
