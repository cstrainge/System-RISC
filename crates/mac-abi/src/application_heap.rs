//! Native application allocator transport. The resident derives the actual
//! application/account from execution; callers cannot select a heap by number.
pub const ALLOCATE: u32 = 0;
pub const DEALLOCATE: u32 = 1;
pub const REALLOCATE: u32 = 2;
pub const ALLOCATE_ZEROED: u32 = 3;
pub const DETAIL_BYTES: usize = 192;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Call
{
    pub operation:     u32,
    pub reserved:      u32,
    pub pointer:       *mut u8,
    pub old_size:      usize,
    pub alignment:     usize,
    pub new_size:      usize,
    /// Output-only: published after successful allocation/reallocation commit.
    /// Null for disposal and every failure; input contents are not authority.
    pub result:        *mut u8,
    /// Output-only bounded UTF-8 failure detail. Success has length zero.
    pub detail_length: u32,
    pub reserved2:     u32,
    pub detail:        [u8; DETAIL_BYTES]
}
impl Default for Call
{
    fn default() -> Self
    {
        Self { operation:     ALLOCATE,
               reserved:      0,
               pointer:       core::ptr::null_mut(),
               old_size:      0,
               alignment:     0,
               new_size:      0,
               result:        core::ptr::null_mut(),
               detail_length: 0,
               reserved2:     0,
               detail:        [0; DETAIL_BYTES] }
    }
}
#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::size_of::<Call>() == 248
                      && core::mem::align_of::<Call>() == 8
                      && core::mem::offset_of!(Call, operation) == 0
                      && core::mem::offset_of!(Call, reserved) == 4
                      && core::mem::offset_of!(Call, pointer) == 8
                      && core::mem::offset_of!(Call, old_size) == 16
                      && core::mem::offset_of!(Call, alignment) == 24
                      && core::mem::offset_of!(Call, new_size) == 32
                      && core::mem::offset_of!(Call, result) == 40
                      && core::mem::offset_of!(Call, detail_length) == 48
                      && core::mem::offset_of!(Call, reserved2) == 52
                      && core::mem::offset_of!(Call, detail) == 56);
