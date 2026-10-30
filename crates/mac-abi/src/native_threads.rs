//! Native Thread Manager. Current names the real THRD, not the process main row.
pub const CURRENT: u32 = 0;
pub const DETAIL_BYTES: usize = 192;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Call
{
    pub operation:     u32,
    pub reserved:      u32,
    pub thread:        u64,
    pub detail_length: u32,
    pub reserved2:     u32,
    pub detail:        [u8; DETAIL_BYTES]
}
impl Default for Call
{
    fn default() -> Self
    {
        Self { operation:     CURRENT,
               reserved:      0,
               thread:        0,
               detail_length: 0,
               reserved2:     0,
               detail:        [0; DETAIL_BYTES] }
    }
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::size_of::<Call>() == 216
                      && core::mem::offset_of!(Call, thread) == 8
                      && core::mem::offset_of!(Call, detail) == 24);
