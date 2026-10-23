//! Native, read-only lifetime inspection. No pointers or caller identity on wire.
pub const BEGIN: u32 = 0;
pub const DOMAIN: u32 = 1;
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
pub struct Domain
{
    pub incarnation:     u64,
    pub kind:            u32,
    pub flags:           u32,
    pub ledger_revision: u64,
    pub kinds:           u64,
    pub live:            u64,
    pub retired:         u64,
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
    pub operation:     u32,
    pub scope:         u32,
    pub revision:      u64,
    pub index:         u64,
    pub count:         u64,
    pub coverage:      u64,
    pub domain:        Domain,
    pub detail:        [u8; DETAIL_BYTES],
    pub detail_length: u32,
    pub reserved:      u32
}
impl Default for Call
{
    fn default() -> Self
    {
        Self { operation:     BEGIN,
               scope:         SELF,
               revision:      0,
               index:         0,
               count:         0,
               coverage:      0,
               domain:        Domain::default(),
               detail:        [0; DETAIL_BYTES],
               detail_length: 0,
               reserved:      0 }
    }
}
