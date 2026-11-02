//! Native Thread Manager. Process and thread outcomes are not service Status.
pub const CURRENT: u32 = 0;
pub const CREATE: u32 = 1;
pub const QUERY: u32 = 2;
pub const JOIN: u32 = 3;
pub const DETACH: u32 = 4;
pub const EXIT: u32 = 5;
pub const YIELD: u32 = 6;
pub const SLEEP: u32 = 7;
pub const NAME: u32 = 8;
pub const PARALLELISM: u32 = 9;
pub const PREPARING: u32 = 0;
pub const RUNNING: u32 = 1;
pub const RETURNING: u32 = 2;
pub const COMPLETE: u32 = 3;
pub const ABORTED: u32 = 4;
pub const NORMAL_OUTCOME: u32 = 0;
pub const ABORTED_OUTCOME: u32 = 1;
pub const DETAIL_BYTES: usize = 192;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Call
{
    pub operation:     u32,
    pub flags:         u32,
    pub reserved:      [u64; 2],
    pub entry:         usize,
    pub data:          usize,
    pub stack_bytes:   u64,
    pub thread:        u64,
    pub right:         u64,
    pub seconds:       u64,
    pub nanoseconds:   u32,
    pub name_length:   u32,
    pub name:          *const u8,
    pub state:         u32,
    pub outcome:       u32,
    pub parallelism:   u32,
    pub reserved2:     u32,
    pub detail_length: u32,
    pub reserved3:     u32,
    pub detail:        [u8; DETAIL_BYTES]
}
impl Default for Call
{
    fn default() -> Self
    {
        Self { operation:     CURRENT,
               flags:         0,
               reserved:      [0; 2],
               entry:         0,
               data:          0,
               stack_bytes:   0,
               thread:        0,
               right:         0,
               seconds:       0,
               nanoseconds:   0,
               name_length:   0,
               name:          core::ptr::null(),
               state:         0,
               outcome:       0,
               parallelism:   0,
               reserved2:     0,
               detail_length: 0,
               reserved3:     0,
               detail:        [0; DETAIL_BYTES] }
    }
}
#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::size_of::<Call>() == 304
                      && core::mem::offset_of!(Call, entry) == 24
                      && core::mem::offset_of!(Call, right) == 56
                      && core::mem::offset_of!(Call, name) == 80
                      && core::mem::offset_of!(Call, detail) == 112);
