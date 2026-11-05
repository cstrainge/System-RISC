//! What a boot screen needs from a display, and nothing more: a framebuffer to
//! draw in. Selection belongs to the caller — the platform record says which
//! display device exists — so nothing here tests an architecture or a board
//! name. The shared drawing code (`Raster`, the icons, the centred panel) takes
//! a [`Frame`] and never learns which backend produced it.
use crate::Raster;

/// A scanout the boot screen may draw into, as the device actually made it.
///
/// `width` and `height` are the geometry the device accepted, which may differ
/// from the one requested; `stride` is the row pitch it reported.
pub struct Frame<'a>
{
    pub width:  usize,
    pub height: usize,
    /// Words per row. Firmware may pad rows, so drawing advances by this, not
    /// by `width`.
    pub stride: usize,
    pub pixels: &'a mut [u32]
}
impl Frame<'_>
{
    /// A raster over this frame, or `None` when its geometry is unusable.
    pub fn raster(&mut self) -> Option<Raster<'_>>
    {
        Raster::strided(self.pixels, self.width, self.height, self.stride)
    }
}

/// Acquire a framebuffer. Implementations differ in how they reach the device;
/// the code that draws does not.
pub trait Backend
{
    type Error: core::fmt::Display;

    /// Bring the scanout up over `pixels` and describe it.
    ///
    /// `pixels` is the caller's pinned storage and stays theirs; the
    /// implementation tells the device its physical address, which is why the
    /// address is the backend's business and not the caller's. The returned
    /// frame borrows the rows the device scans out.
    fn acquire<'a>(&mut self, pixels: &'a mut [u32]) -> Result<Frame<'a>, Self::Error>;
}
