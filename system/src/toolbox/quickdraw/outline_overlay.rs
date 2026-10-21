//! Transient tracking feedback shares the cursor's save-under presentation.
//! The backbuffer and window cache always contain the underlying scene alone.
#![forbid(unsafe_code)]
use super::{Canvas, Rect, geometry::sect_rect, region::Region};
use crate::toolbox::windows::WindowManager;
const PIXELS: usize = 2 * (640 + 480);
#[derive(Clone, Copy)]
struct Saved
{
    bounds: Rect,
    offset: usize
}
pub struct OutlineUnder
{
    pixels: [u32; PIXELS],
    pieces: [Saved; PIXELS],
    length: usize,
    used:   usize
}
impl Default for OutlineUnder
{
    fn default() -> Self
    {
        Self { pixels: [0; PIXELS],
               pieces: [Saved { bounds: Rect::ZERO,
                               offset: 0 }; PIXELS],
               length: 0,
               used:   0 }
    }
}
impl OutlineUnder
{
    /// Exactly one active target owns tracking feedback. Desktop feedback is
    /// below every window; window feedback is clipped by higher structures.
    pub fn paint(&mut self, canvas: &mut Canvas<'_>, windows: &WindowManager,
                 desktop: Option<Rect>, damage: &Region)
    {
        self.length = 0;
        self.used = 0;
        if let Some(window) = windows.active_window().and_then(|id| windows.window(id))
        {
            let Some(mut outline) = window.outline_rect()
            else
            {
                return;
            };
            outline.offset(window.bounds.left, window.bounds.top);
            windows.for_each_visible_rect(window.id, |visible| {
                       self.paint_piece(canvas, outline, visible, damage)
                   });
        }
        else if let Some(outline) = desktop
        {
            windows.for_each_desktop_rect(Rect::new(0, 21, canvas.width as i16,
                                                    canvas.height as i16),
                                          |visible| {
                                              self.paint_piece(canvas, outline, visible, damage)
                                          });
        }
    }

    fn paint_piece(&mut self, canvas: &mut Canvas<'_>, outline: Rect, visible: Rect,
                   damage: &Region)
    {
        for edge in super::geometry::outline_edges(outline)
        {
            let mut edge_clip = Rect::ZERO;
            if !sect_rect(edge, visible, &mut edge_clip)
            {
                continue;
            }
            for band in damage.bands()
            {
                if band.bottom <= edge_clip.top
                {
                    continue;
                }
                if band.top >= edge_clip.bottom
                {
                    break;
                }
                for span in &band.spans
                {
                    let mut rect = Rect::ZERO;
                    if sect_rect(edge_clip,
                                 Rect::new(span.left, band.top, span.right, band.bottom),
                                 &mut rect)
                    {
                        self.capture(canvas, rect);
                    }
                }
            }
        }
    }

    fn capture(&mut self, canvas: &mut Canvas<'_>, rect: Rect)
    {
        let count = (rect.right - rect.left) as usize;
        let size = count * (rect.bottom - rect.top) as usize;
        // Screen clipping and the one-outline perimeter bound establish these
        // limits. Do not allocate or grow a paint history on pointer motion.
        assert!(self.used + size <= PIXELS && self.length < PIXELS);
        self.pieces[self.length] = Saved { bounds: rect,
                                           offset: self.used };
        self.length += 1;
        for y in rect.top..rect.bottom
        {
            let at = y as usize * canvas.width + rect.left as usize;
            self.pixels[self.used..self.used + count].copy_from_slice(&canvas.pixels
                                                                          [at..at + count]);
            self.used += count;
        }
        canvas.fill_rect(rect, [0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55], 0, 0xffffff);
    }

    pub fn restore(&mut self, canvas: &mut Canvas<'_>)
    {
        for saved in &self.pieces[..self.length]
        {
            let count = (saved.bounds.right - saved.bounds.left) as usize;
            for (row, y) in (saved.bounds.top..saved.bounds.bottom).enumerate()
            {
                let at = y as usize * canvas.width + saved.bounds.left as usize;
                let offset = saved.offset + row * count;
                canvas.pixels[at..at + count].copy_from_slice(&self.pixels[offset..offset + count]);
            }
        }
        self.length = 0;
        self.used = 0;
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn temporary_outline_pixels_restore_exactly_without_growing_history()
    {
        let original: alloc::vec::Vec<_> = (0..200 * 100).map(|n| n as u32 & 0xffffff).collect();
        let mut pixels = original.clone();
        let mut overlay = OutlineUnder::default();
        let mut canvas = Canvas::new(&mut pixels, 200, 100);
        for step in 0..2000
        {
            let rect = Rect::new(4, 4, 40 + (step % 120) as i16, 80);
            for edge in super::super::geometry::outline_edges(rect)
            {
                if !edge.is_empty()
                {
                    overlay.capture(&mut canvas, edge);
                }
            }
            assert!(overlay.used <= 2 * (200 + 100));
            overlay.restore(&mut canvas);
            assert_eq!(canvas.pixels, original.as_slice());
            assert_eq!(overlay.used, 0);
            assert_eq!(overlay.length, 0);
        }
    }
}
