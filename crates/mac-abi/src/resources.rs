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
pub const FILE_VERSION: u32 = 11;
/// Decoded vers strings. The disk resource remains a classic NumVersion and
/// two MacRoman Pascal strings; native clients receive checked UTF8.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VersionInfo
{
    pub number:       [u8; 4],
    pub country:      u16,
    pub reserved:     u16,
    pub short_length: u32,
    pub long_length:  u32,
    pub short:        [u8; crate::INLINE_STRING_BYTES],
    pub long:         [u8; crate::INLINE_STRING_BYTES]
}
impl Default for VersionInfo
{
    fn default() -> Self
    {
        Self { number:       [0; 4],
               country:      0,
               reserved:     0,
               short_length: 0,
               long_length:  0,
               short:        [0; crate::INLINE_STRING_BYTES],
               long:         [0; crate::INLINE_STRING_BYTES] }
    }
}
impl VersionInfo
{
    pub fn short(&self) -> &str
    {
        core::str::from_utf8(&self.short[..(self.short_length as usize).min(self.short.len())]).unwrap_or("")
    }

    pub fn long(&self) -> &str
    {
        core::str::from_utf8(&self.long[..(self.long_length as usize).min(self.long.len())]).unwrap_or("")
    }
}
/// Distinct size preserves the existing ResourceCall ABI and scope semantics.
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct FileVersionCall
{
    pub operation: u32,
    pub volume:    u32,
    pub cnid:      u32,
    pub id:        i16,
    pub reserved:  u16,
    pub info:      VersionInfo
}
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

/// Read-only inspection through an existing File Manager resource-fork handle.
/// No resource-chain mutation, executable loading, or additional object identity.
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct FileResourceCall
{
    pub operation:   u32,
    pub handle:      u32,
    pub key:         ResourceKey,
    pub index:       u32,
    pub info:        ResourceInfo,
    pub has_name:    u32,
    pub name:        *const u8,
    pub name_length: usize,
    pub output:      *mut u8,
    pub capacity:    usize,
    pub offset:      usize,
    pub transferred: usize,
    pub reserved:    [u64; 2]
}
