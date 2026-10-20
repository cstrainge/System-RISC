//! One bounded snapshot of the foreground menu bar, independent of windows.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use super::Canvas;
pub const BYTES: usize = 640 * 21 * 4;

#[derive(Default)]
pub struct MenuBarCache
{
    key:        Option<(u64, Option<i16>, bool)>,
    pixels:     Vec<u32>,
    pub paints: u64
}
impl MenuBarCache
{
    pub fn invalidate(&mut self)
    {
        self.key = None;
    }

    /// The full compositor supplies screen coordinates and a complete surface.
    /// Popups remain separate overlays; hover rows cannot invalidate bar pixels.
    pub fn paint(&mut self, canvas: &mut Canvas<'_>, key: (u64, Option<i16>, bool),
                 draw: impl FnOnce(&mut Canvas<'_>))
    {
        let count = BYTES / 4;
        if self.key == Some(key) && self.pixels.len() == count
        {
            canvas.pixels[..count].copy_from_slice(&self.pixels);
            return;
        }
        draw(canvas);
        self.paints = self.paints.wrapping_add(1);
        if self.pixels.is_empty()
        {
            if self.pixels.try_reserve_exact(count).is_err()
            {
                return;
            }
            self.pixels.resize(count, 0);
        }
        self.pixels.copy_from_slice(&canvas.pixels[..count]);
        self.key = Some(key);
    }
}
