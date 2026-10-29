//! Public discovered CPU metadata and restore-commit residency observations.
//! Ordinals are positions in immutable topology, never resident object IDs.
pub const BEGIN: u32 = 9;
pub const READ: u32 = 10;
pub const DISCOVERED: u32 = 0;
pub const ADMITTING: u32 = 1;
pub const ADMITTED: u32 = 2;
pub const IDLE: u32 = 3;
pub const EXECUTING: u32 = 4;
pub const STOPPED: u32 = 5;
pub const PRIMARY: u32 = 1;
pub const IS_ADMITTED: u32 = 2;
pub const DISPATCH_ELIGIBLE: u32 = 4;
pub const ACTIVITY_UNINITIALIZED: u32 = 0;
pub const ACTIVITY_RESIDENCY: u32 = 1;
pub const ACTIVITY_STOPPED: u32 = 2;
pub const ACTIVITY_CLOCK_REGRESSED: u32 = 3;
pub const ACTIVITY_OVERFLOW: u32 = 4;
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary
{
    pub count:            u64,
    pub ticks_per_second: u64
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Row
{
    pub hart:            u64,
    pub idle_ticks:      u64,
    pub executing_ticks: u64,
    pub sampled_at:      u64,
    pub state:           u32,
    pub flags:           u32,
    pub activity:        u32,
    pub reserved:        u32
}
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Call
{
    pub operation:     u32,
    pub reserved:      u32,
    pub index:         u64,
    pub summary:       Summary,
    pub row:           Row,
    pub detail:        [u8; super::DETAIL_BYTES],
    pub detail_length: u32,
    pub reserved_tail: u32
}
impl Default for Call
{
    fn default() -> Self
    {
        Self { operation:     BEGIN,
               reserved:      0,
               index:         0,
               summary:       Summary::default(),
               row:           Row::default(),
               detail:        [0; super::DETAIL_BYTES],
               detail_length: 0,
               reserved_tail: 0 }
    }
}
const _: () = assert!(core::mem::size_of::<Summary>() == 16
                      && core::mem::size_of::<Row>() == 48
                      && core::mem::size_of::<Call>() == 280);

// Real wire invariants, independent of the deferred host test framework.
#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::size_of::<Call>() == 280
                      && core::mem::offset_of!(Call, row) == 32
                      && core::mem::offset_of!(Call, detail) == 80
                      && core::mem::offset_of!(Call, detail_length) == 272
                      && core::mem::size_of::<Summary>() == 16
                      && core::mem::size_of::<Row>() == 48);
