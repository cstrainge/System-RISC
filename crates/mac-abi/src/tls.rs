//! Process-owned TLS keys and actual-thread slots. Values are opaque, including
//! zero and the Rust std sentinels one and two. Callbacks run only in app CRT.
pub const CREATE: u32 = 0;
pub const DELETE: u32 = 1;
pub const GET: u32 = 2;
pub const SET: u32 = 3;
pub const BEGIN: u32 = 4;
pub const NEXT: u32 = 5;
pub const ACK: u32 = 6;
pub const FINISH: u32 = 7;
pub const DESTRUCTOR_ROUNDS: u32 = 8;
pub const DETAIL_BYTES: usize = 192;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Call
{
    pub operation:     u32,
    pub flags:         u32,
    pub reserved:      [u64; 2],
    pub key:           u64,
    pub value:         usize,
    pub destructor:    usize,
    pub ticket:        u64,
    pub present:       u32,
    pub round:         u32,
    pub detail_length: u32,
    pub reserved2:     u32,
    pub detail:        [u8; DETAIL_BYTES]
}
impl Default for Call
{
    fn default() -> Self
    {
        Self { operation:     CREATE,
               flags:         0,
               reserved:      [0; 2],
               key:           0,
               value:         0,
               destructor:    0,
               ticket:        0,
               present:       0,
               round:         0,
               detail_length: 0,
               reserved2:     0,
               detail:        [0; DETAIL_BYTES] }
    }
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::size_of::<Call>() == 264
                      && core::mem::offset_of!(Call, key) == 24
                      && core::mem::offset_of!(Call, ticket) == 48
                      && core::mem::offset_of!(Call, detail) == 72);
