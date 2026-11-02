//! Native atomic-address waiting. The caller keeps the AtomicU32 object alive;
//! the System retains its admitted backing until the wait has been detached.
pub const WAIT32: u32 = 0;
pub const WAKE32: u32 = 1;
pub const FINITE: u32 = 1;
pub const WAKE_ALL: u32 = 2;
pub const WOKEN: u32 = 1;
pub const VALUE_CHANGED: u32 = 2;
pub const TIMED_OUT: u32 = 3;
pub const DETAIL_BYTES: usize = 192;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Call
{
    pub operation:     u32,
    pub flags:         u32,
    pub address:       usize,
    pub expected:      u32,
    pub nanoseconds:   u32,
    pub seconds:       u64,
    pub reserved:      [u64; 2],
    pub outcome:       u32,
    pub awakened:      u32,
    pub detail_length: u32,
    pub reserved2:     u32,
    pub detail:        [u8; DETAIL_BYTES]
}

impl Default for Call
{
    fn default() -> Self
    {
        Self { operation:     WAIT32,
               flags:         0,
               address:       0,
               expected:      0,
               nanoseconds:   0,
               seconds:       0,
               reserved:      [0; 2],
               outcome:       0,
               awakened:      0,
               detail_length: 0,
               reserved2:     0,
               detail:        [0; DETAIL_BYTES] }
    }
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::size_of::<Call>() == 256
                      && core::mem::offset_of!(Call, address) == 8
                      && core::mem::offset_of!(Call, seconds) == 24
                      && core::mem::offset_of!(Call, outcome) == 48
                      && core::mem::offset_of!(Call, detail) == 64);
