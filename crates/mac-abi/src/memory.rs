//! Native Memory Manager requests. Handles are opaque IDs, not classic pointers.
pub const NEW_CLEAR: u32 = 0;
pub const DISPOSE: u32 = 1;
pub const SIZE: u32 = 2;
pub const RESIZE: u32 = 3;
pub const STATE: u32 = 4;
pub const SET_STATE: u32 = 5;
pub const LOCK: u32 = 6;
pub const UNLOCK: u32 = 7;
pub const PURGEABLE: u32 = 8;
pub const NONPURGEABLE: u32 = 9;
pub const EMPTY: u32 = 10;
pub const REALLOCATE: u32 = 11;
pub const DUPLICATE: u32 = 12;
pub const FROM_BYTES: u32 = 13;
pub const APPEND_BYTES: u32 = 14;
pub const APPEND_HANDLE: u32 = 15;
pub const READ: u32 = 16;
pub const WRITE: u32 = 17;
pub const ERROR: u32 = 18;
pub const NEW_EMPTY: u32 = 19;
pub const REPLACE_BYTES: u32 = 20;

pub const LOCKED: u8 = 0x80;
pub const PURGEABLE_BIT: u8 = 0x40;
pub const RESOURCE: u8 = 0x20;
// Original Memory Manager errors, from pinned Multiversal interfaces.
pub const MEM_FULL_ERR: i32 = -108;
pub const NIL_HANDLE_ERR: i32 = -109;
pub const MEM_WZ_ERR: i32 = -111;
pub const MEM_PUR_ERR: i32 = -112;
pub const MEM_LOCKED_ERR: i32 = -117;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MemoryCall
{
    pub operation:    u32,
    pub handle:       u32,
    pub other:        u32,
    pub size:         u32,
    pub offset:       u32,
    pub state:        u32,
    pub input:        *const u8,
    pub input_length: usize,
    pub output:       *mut u8,
    pub capacity:     usize,
    pub length:       usize,
    pub error:        i32
}
impl Default for MemoryCall
{
    fn default() -> Self
    {
        Self { operation:    0,
               handle:       0,
               other:        0,
               size:         0,
               offset:       0,
               state:        0,
               input:        core::ptr::null(),
               input_length: 0,
               output:       core::ptr::null_mut(),
               capacity:     0,
               length:       0,
               error:        0 }
    }
}
