//! Owner-scoped, one-shot Time Manager tasks. No routine pointer enters System.
pub const INSERT: u32 = 0;
pub const PRIME: u32 = 1;
pub const FIRE: u32 = 2;
pub const REMOVE: u32 = 3;
/// Native event-loop rescheduling: preserve the installed identity, restart
/// its schedule from now, and wake observers to recompute their park deadline.
/// Classic PrimeTime remains strict; its active-task rule is unchanged.
pub const RESET: u32 = 4;
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct Call
{
    pub operation: u32,
    pub extended:  u32,
    pub id:        u64,
    pub delay:     i32,
    pub state:     u32,
    pub deadline:  u64,
    pub remaining: u64,
    pub anchor:    u64
}
#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::size_of::<Call>() == 48
                      && core::mem::align_of::<Call>() == 8
                      && core::mem::offset_of!(Call, id) == 8
                      && core::mem::offset_of!(Call, deadline) == 24);
