//! Read-only native Resource Manager contracts for resident resource forks.
use crate::ResourceKey;
pub const CHAIN: i16 = -1;
pub const SYSTEM: i16 = 0;
pub const APPLICATION: i16 = 1;
/// Read-only pinned image scopes advertised by resident services. They never
/// select an execution identity or grant access to mutable application objects.
pub const LOADED_IMAGE_BASE: i16 = 256;
pub const GET: u32 = 0;
pub const NAMED: u32 = 1;
pub const COUNT: u32 = 2;
pub const INDEX: u32 = 3;
pub const COUNT_TYPES: u32 = 4;
pub const TYPE_INDEX: u32 = 5;
pub const INFO: u32 = 6;
pub const READ: u32 = 7;
pub const RELEASE: u32 = 8;
/// Decode STR / zero-based STR# element to native UTF8 in output.
pub const STRING: u32 = 10;
pub const LOAD: u32 = 9;
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ResourceInfo
{
    pub key:         ResourceKey,
    pub origin:      i16,
    pub attributes:  u16,
    pub length:      usize,
    pub name_length: u32,
    pub name:        [u8; crate::INLINE_STRING_BYTES]
}
impl Default for ResourceInfo
{
    fn default() -> Self
    {
        Self { key:         ResourceKey { kind: [0; 4],
                                          id:   0 },
               origin:      CHAIN,
               attributes:  0,
               length:      0,
               name_length: 0,
               name:        [0; crate::INLINE_STRING_BYTES] }
    }
}
impl ResourceInfo
{
    pub fn name(&self) -> &str
    {
        core::str::from_utf8(&self.name[..(self.name_length as usize).min(self.name.len())]).unwrap_or("")
    }
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct ResourceCall
{
    pub operation:   u32,
    pub scope:       i16,
    pub reserved:    u16,
    pub key:         ResourceKey,
    pub index:       u32,
    pub name:        *const u8,
    pub name_length: usize,
    pub info:        ResourceInfo,
    pub output:      *mut u8,
    pub capacity:    usize,
    pub offset:      usize,
    pub transferred: usize
}
