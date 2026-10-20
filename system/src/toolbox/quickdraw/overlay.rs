//! Temporary cursor pixels never become part of the retained scene.
#![forbid(unsafe_code)]
use super::{Canvas, Rect, geometry::sect_rect};

pub struct CursorUnder
{
    bounds: Rect,
    pixels: [u32; 256]
}
impl CursorUnder
{
    pub fn capture(canvas: &Canvas<'_>, bounds: Rect) -> Self
    {
        let mut result = Self { bounds: Rect::ZERO,
                                pixels: [0; 256] };
        sect_rect(bounds,
                  Rect::new(0, 0, canvas.width as i16, canvas.height as i16),
                  &mut result.bounds);
        let width = (result.bounds.right - result.bounds.left) as usize;
        assert!(width <= 16 && result.bounds.bottom - result.bounds.top <= 16);
        for (row, y) in (result.bounds.top..result.bounds.bottom).enumerate()
        {
            let at = y as usize * canvas.width + result.bounds.left as usize;
            result.pixels[row * width..(row + 1) * width].copy_from_slice(&canvas.pixels
                                                                              [at..at + width]);
        }
        result
    }

    pub fn restore(self, canvas: &mut Canvas<'_>)
    {
        let width = (self.bounds.right - self.bounds.left) as usize;
        for (row, y) in (self.bounds.top..self.bounds.bottom).enumerate()
        {
            let at = y as usize * canvas.width + self.bounds.left as usize;
            canvas.pixels[at..at + width].copy_from_slice(&self.pixels
                                                              [row * width..(row + 1) * width]);
        }
    }
}
