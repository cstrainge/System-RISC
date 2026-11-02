//! Native entropy service. The resident broker owns staging while the actual
//! caller parks; this synchronous call never transfers its output allocation.
pub const READY: u32 = 0;
pub const FILL: u32 = 1;
pub const MAX_CAPACITY: usize = 65_536;
pub const DETAIL_BYTES: usize = 256;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Call
{
    pub operation:     u32,
    pub flags:         u32,
    pub reserved:      [u64; 2],
    pub output:        *mut u8,
    pub capacity:      u32,
    pub written:       u32,
    pub detail_length: u32,
    pub reserved2:     u32,
    pub detail:        [u8; DETAIL_BYTES]
}
impl Default for Call
{
    fn default() -> Self
    {
        Self { operation:     READY,
               flags:         0,
               reserved:      [0; 2],
               output:        core::ptr::null_mut(),
               capacity:      0,
               written:       0,
               detail_length: 0,
               reserved2:     0,
               detail:        [0; DETAIL_BYTES] }
    }
}
#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::size_of::<Call>() == 304
                      && core::mem::offset_of!(Call, output) == 24
                      && core::mem::offset_of!(Call, detail) == 48);
