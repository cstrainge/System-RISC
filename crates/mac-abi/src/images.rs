//! Bounded resident PNG decoding. Pointers are valid only for the synchronous call.
use crate::{Rect, Status, Surface};
pub const INFO: u32 = 0;
pub const DRAW: u32 = 1;
pub const HAS_ALPHA: u16 = 1;
pub const MAX_INPUT_BYTES: usize = 1024 * 1024;
pub const MAX_PIXELS: usize = 262_144;
pub const MAX_DIMENSION: u32 = 1024;
pub const BAD_SIGNATURE: u32 = 1;
pub const TRUNCATED: u32 = 2;
pub const INVALID_HEADER: u32 = 3;
pub const INVALID_CHUNK: u32 = 4;
pub const CRC_MISMATCH: u32 = 5;
pub const BAD_COMPRESSED_DATA: u32 = 6;
pub const INVALID_PALETTE: u32 = 7;
pub const UNSUPPORTED: u32 = 8;
pub const LIMIT_EXCEEDED: u32 = 9;
pub const ALLOCATION_FAILED: u32 = 10;
pub const INVALID_DESTINATION: u32 = 11;
pub const SERVICE_ERROR: u32 = 12;
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageInfo
{
    pub width:      u32,
    pub height:     u32,
    pub bit_depth:  u8,
    pub color_type: u8,
    pub flags:      u16
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageError
{
    pub status: Status,
    pub kind:   u32
}
impl core::fmt::Display for ImageError
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        write!(f, "PNG service error {} (detail {})", self.status, self.kind)
    }
}
impl core::error::Error for ImageError
{
}
pub type Result<T> = core::result::Result<T, ImageError>;
#[repr(C)]
#[derive(Default)]
pub struct ImageCall
{
    pub operation:   u32,
    pub window:      u32,
    pub data:        *const u8,
    pub length:      usize,
    pub surface:     *mut Surface,
    pub destination: Rect,
    pub info:        ImageInfo,
    pub error_kind:  u32
}
