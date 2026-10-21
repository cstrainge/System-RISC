//! PNG bytes cross the gate; decoding, scaling and alpha composition run in System.
//! Static noninterlaced PNGs are bounded by MAX_INPUT_BYTES/MAX_PIXELS. APNG and
//! Adam7 return NOT_SUPPORTED. Metadata color profiles are not applied.
pub use mac_abi::images::{MAX_DIMENSION, MAX_INPUT_BYTES, MAX_PIXELS};
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind
{
    BadSignature,
    Truncated,
    InvalidHeader,
    InvalidChunk,
    CrcMismatch,
    BadCompressedData,
    InvalidPalette,
    Unsupported,
    LimitExceeded,
    AllocationFailed,
    InvalidDestination,
    ServiceError,
    Unknown(u32)
}
impl From<u32> for ErrorKind
{
    fn from(kind: u32) -> Self
    {
        match kind
        {
            mac_abi::images::BAD_SIGNATURE => Self::BadSignature,
            mac_abi::images::TRUNCATED => Self::Truncated,
            mac_abi::images::INVALID_HEADER => Self::InvalidHeader,
            mac_abi::images::INVALID_CHUNK => Self::InvalidChunk,
            mac_abi::images::CRC_MISMATCH => Self::CrcMismatch,
            mac_abi::images::BAD_COMPRESSED_DATA => Self::BadCompressedData,
            mac_abi::images::INVALID_PALETTE => Self::InvalidPalette,
            mac_abi::images::UNSUPPORTED => Self::Unsupported,
            mac_abi::images::LIMIT_EXCEEDED => Self::LimitExceeded,
            mac_abi::images::ALLOCATION_FAILED => Self::AllocationFailed,
            mac_abi::images::INVALID_DESTINATION => Self::InvalidDestination,
            mac_abi::images::SERVICE_ERROR => Self::ServiceError,
            unknown => Self::Unknown(unknown)
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageError
{
    pub service: crate::Error,
    pub kind:    ErrorKind
}
impl From<mac_abi::images::ImageError> for ImageError
{
    fn from(error: mac_abi::images::ImageError) -> Self
    {
        Self { service: crate::Error::from_status(error.status),
               kind:    error.kind.into() }
    }
}
impl core::fmt::Display for ImageError
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        write!(f, "PNG {:?}: {}", self.kind, self.service)
    }
}
impl core::error::Error for ImageError
{
}
pub type Result<T> = core::result::Result<T, ImageError>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageInfo
{
    pub width:      u32,
    pub height:     u32,
    pub bit_depth:  u8,
    pub color_type: u8,
    pub has_alpha:  bool
}
impl From<mac_abi::images::ImageInfo> for ImageInfo
{
    fn from(info: mac_abi::images::ImageInfo) -> Self
    {
        Self { width:      info.width,
               height:     info.height,
               bit_depth:  info.bit_depth,
               color_type: info.color_type,
               has_alpha:  info.flags & mac_abi::images::HAS_ALPHA != 0 }
    }
}

use crate::{Client, Rect, graphics::GraphicsPort};
#[derive(Clone, Copy)]
pub struct Images<'a>
{
    client: Client<'a>
}
impl<'a> Images<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    /// Validate PNG framing/CRCs and report IHDR metadata without inflating pixels.
    pub fn info(self, png: &[u8]) -> Result<ImageInfo>
    {
        self.client.png_info(png).map(Into::into).map_err(Into::into)
    }

    /// Synchronous drawing into an opaque RGB24 surface; nearest-neighbor scaling
    /// and straight-alpha source-over in encoded channel values. No partial draw
    /// occurs on malformed input or an allocation error.
    pub fn draw_to_surface(self, png: &[u8], pixels: &mut [u32], width: usize, height: usize,
                           destination: Rect, clip: Rect)
                           -> Result<()>
    {
        self.client.png_pixels(png, pixels, width, height, destination, clip).map_err(Into::into)
    }
}
impl<'a> crate::QuickDraw<'a>
{
    pub fn images(self) -> Images<'a>
    {
        Images::new(self.client)
    }
}
impl GraphicsPort<'_>
{
    pub fn draw_png(self, png: &[u8], destination: Rect) -> Result<()>
    {
        self.client.png_draw(self.window.0, png, destination).map_err(Into::into)
    }
}
impl crate::SurfacePort<'_>
{
    pub fn draw_png(&mut self, png: &[u8], destination: Rect) -> Result<()>
    {
        self.connection.png(png, destination).map_err(Into::into)
    }
}
