//! Byte-transport pipes. A pipe carries bytes in and bytes out; it has no
//! framing, record boundary or message operation. Handles are opaque,
//! generation-tagged and owned by the process that received them.
pub const CREATE: u32 = 0;
pub const CREATE_NAMED: u32 = 1;
pub const CONNECT: u32 = 2;
pub const READ: u32 = 3;
pub const WRITE: u32 = 4;
pub const SET_BLOCKING: u32 = 5;
pub const CLOSE: u32 = 6;

/// Capacity bounds. The lower bound keeps a maximum-size atomic write able to
/// fit; the upper bound is the v1 storage policy.
pub const MIN_CAPACITY: usize = 512;
pub const MAX_CAPACITY: usize = 65_536;
/// Resident pipe limit, shared by anonymous and named pipes.
pub const MAX_PIPES: usize = 64;
/// A write of at most this size is indivisible against concurrent writers.
/// It is an atomicity bound only and is never a record size.
pub const ATOMIC_WRITE: usize = 512;
/// HFS item-name bound, because a named pipe is also a Finder item.
pub const NAME_BYTES: usize = 31;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Call
{
    pub operation:   u32,
    /// Zero is blocking; one is non-blocking. Initial mode on create, new mode on `SET_BLOCKING`.
    pub nonblocking: u32,
    pub handle:      u64,
    pub capacity:    u32,
    pub name_length: u32,
    pub buffer:      *mut u8,
    pub length:      usize,
    pub transferred: usize,
    pub reader:      u64,
    pub writer:      u64,
    pub name:        [u8; NAME_BYTES],
    pub reserved:    [u64; 2]
}
impl Default for Call
{
    fn default() -> Self
    {
        Self { operation:   0,
               nonblocking: 0,
               handle:      0,
               capacity:    0,
               name_length: 0,
               buffer:      core::ptr::null_mut(),
               length:      0,
               transferred: 0,
               reader:      0,
               writer:      0,
               name:        [0; NAME_BYTES],
               reserved:    [0; 2] }
    }
}
#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::size_of::<Call>() == 112
                      && core::mem::align_of::<Call>() == 8
                      && core::mem::offset_of!(Call, handle) == 8
                      && core::mem::offset_of!(Call, capacity) == 16
                      && core::mem::offset_of!(Call, buffer) == 24
                      && core::mem::offset_of!(Call, length) == 32
                      && core::mem::offset_of!(Call, transferred) == 40
                      && core::mem::offset_of!(Call, reader) == 48
                      && core::mem::offset_of!(Call, writer) == 56
                      && core::mem::offset_of!(Call, name) == 64
                      && core::mem::offset_of!(Call, reserved) == 96);
