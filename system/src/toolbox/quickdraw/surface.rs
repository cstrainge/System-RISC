use mac_abi::Surface;

use super::Canvas;
pub trait SurfaceExt
{
    fn from_canvas(canvas: &mut Canvas<'_>) -> Surface;
    /// # Safety
    /// The surface must borrow live, exclusive and sufficiently large pixel storage.
    unsafe fn canvas(&mut self) -> Option<Canvas<'_>>;
}

impl SurfaceExt for Surface
{
    fn from_canvas(canvas: &mut Canvas<'_>) -> Surface
    {
        Surface { pixels: canvas.pixels.as_mut_ptr(),
                  width:  canvas.width as u32,
                  height: canvas.height as u32,
                  origin: canvas.origin(),
                  clip:   canvas.clip() }
    }

    /// # Safety
    /// Pixels must be aligned, exclusively writable and contain width*height
    /// initialized pixels for the returned borrow. No concurrent drawing is allowed.
    unsafe fn canvas(&mut self) -> Option<Canvas<'_>>
    {
        let length = (self.width as usize).checked_mul(self.height as usize)?;
        if self.pixels.is_null()
           || !(self.pixels as usize).is_multiple_of(4)
           || length > crate::display::PIXELS
           || self.width == 0
           || self.height == 0
           || self.width > i16::MAX as u32
           || self.height > i16::MAX as u32
        {
            return None;
        }
        let pixels = unsafe { core::slice::from_raw_parts_mut(self.pixels, length) };
        let mut canvas = Canvas::new(pixels, self.width as usize, self.height as usize);
        canvas.set_clip(self.clip);
        canvas.set_origin(self.origin.h, self.origin.v);
        Some(canvas)
    }
}
