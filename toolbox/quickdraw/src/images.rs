//! PNG bytes cross the gate; decoding, scaling and alpha composition run in System.
//! Static noninterlaced PNGs are bounded by MAX_INPUT_BYTES/MAX_PIXELS. APNG and
//! Adam7 return NOT_SUPPORTED. Metadata color profiles are not applied.
pub use mac_abi::{images as errors,
                  images::{HAS_ALPHA, ImageError, ImageInfo, MAX_DIMENSION, MAX_INPUT_BYTES,
                           MAX_PIXELS, Result}};

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
        self.client.png_info(png)
    }

    /// Synchronous drawing into an opaque RGB24 surface; nearest-neighbor scaling
    /// and straight-alpha source-over in encoded channel values. No partial draw
    /// occurs on malformed input or an allocation error.
    pub fn draw_to_surface(self, png: &[u8], pixels: &mut [u32], width: usize, height: usize,
                           destination: Rect, clip: Rect)
                           -> Result<()>
    {
        self.client.png_pixels(png, pixels, width, height, destination, clip)
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
        self.client.png_draw(self.window.0, png, destination)
    }
}
impl crate::SurfacePort<'_>
{
    pub fn draw_png(&mut self, png: &[u8], destination: Rect) -> Result<()>
    {
        self.connection.png(png, destination)
    }
}
