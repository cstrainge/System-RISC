//! Bounded acceleration for committed retained window drawing.
//!
//! Populate from the real screen after the ordinary clipped replay. Rendering
//! into a small local surface changes global pattern phase and can change source
//! clipping, so this cache never substitutes a different rendering surface.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use super::{Canvas, Point, Rect, geometry::sect_rect};

const MAX_FRAGMENTS: usize = 64;
const MAX_WINDOWS: usize = 32;
pub const PIXEL_BUDGET: usize = 2 * 1024 * 1024;

struct Entry
{
    id:        u32,
    bounds:    Rect,
    origin:    Point,
    screen:    (usize, usize),
    crop:      Rect,
    pixels:    Vec<u32>,
    valid:     [Rect; MAX_FRAGMENTS],
    valid_len: usize,
    touched:   u64,
    frame:     u64
}

impl Entry
{
    fn matches(&self, bounds: Rect, canvas: &Canvas<'_>) -> bool
    {
        self.bounds == bounds
        && self.origin == canvas.origin()
        && self.screen == (canvas.width, canvas.height)
    }

    fn contains(&self, rect: Rect) -> bool
    {
        self.valid[..self.valid_len].iter().any(|valid| {
                                               valid.left <= rect.left
                                               && valid.top <= rect.top
                                               && valid.right >= rect.right
                                               && valid.bottom >= rect.bottom
                                           })
    }

    fn copy(&mut self, canvas: &mut Canvas<'_>, rect: Rect, save: bool)
    {
        let width = (self.crop.right - self.crop.left) as usize;
        let count = (rect.right - rect.left) as usize;
        for y in rect.top as usize..rect.bottom as usize
        {
            let screen = y * canvas.width + rect.left as usize;
            let cached =
                (y - self.crop.top as usize) * width + (rect.left - self.crop.left) as usize;
            if save
            {
                self.pixels[cached..cached + count].copy_from_slice(&canvas.pixels
                                                                        [screen..screen + count]);
            }
            else
            {
                canvas.pixels[screen..screen + count].copy_from_slice(&self.pixels
                                                                          [cached..cached + count]);
            }
        }
        if save && self.valid_len < MAX_FRAGMENTS
        {
            self.valid[self.valid_len] = rect;
            self.valid_len += 1;
        }
    }
}

#[derive(Default)]
pub struct ContentCache
{
    entries: Vec<Entry>,
    clock:   u64,
    frame:   u64
}

impl ContentCache
{
    pub fn begin_frame(&mut self)
    {
        self.frame = self.frame.wrapping_add(1);
    }

    /// Keep allocated storage across commits; pending drawing cannot invalidate
    /// the old committed scene until EndUpdate publishes its replacement.
    pub fn invalidate(&mut self, id: u32)
    {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id)
        {
            entry.valid_len = 0;
        }
    }

    pub fn retain(&mut self, mut live: impl FnMut(u32) -> bool)
    {
        self.entries.retain(|entry| live(entry.id));
    }

    fn bytes(&self) -> usize
    {
        self.entries.iter().map(|entry| entry.pixels.capacity() * 4).sum()
    }

    /// A miss always replays normally, including allocation failure or an
    /// oversized window. At most 64 visible fragments per window are retained;
    /// further fragments safely replay rather than growing metadata unbounded.
    pub fn paint(&mut self, id: u32, bounds: Rect, canvas: &mut Canvas<'_>,
                 replay: impl FnOnce(&mut Canvas<'_>))
    {
        let mut crop = Rect::ZERO;
        let mut visible = Rect::ZERO;
        if !sect_rect(bounds, Rect::new(0, 0, canvas.width as i16, canvas.height as i16), &mut crop)
           || !sect_rect(crop, canvas.clip(), &mut visible)
        {
            return;
        }
        self.clock = self.clock.wrapping_add(1);
        if let Some(index) = self.entries.iter().position(|entry| entry.id == id)
        {
            if !self.entries[index].matches(bounds, canvas)
            {
                self.entries.swap_remove(index);
            }
            else if self.entries[index].contains(visible)
            {
                self.entries[index].touched = self.clock;
                self.entries[index].frame = self.frame;
                self.entries[index].copy(canvas, visible, false);
                return;
            }
        }

        replay(canvas);

        let index = if let Some(index) = self.entries.iter().position(|entry| entry.id == id)
        {
            index
        }
        else
        {
            let count = (crop.right - crop.left) as usize * (crop.bottom - crop.top) as usize;
            let bytes = count * core::mem::size_of::<u32>();
            if bytes > PIXEL_BUDGET
            {
                return;
            }
            while self.entries.len() >= MAX_WINDOWS || self.bytes() + bytes > PIXEL_BUDGET
            {
                // Preserve the hot background if the visible scene exceeds
                // the budget, rather than cycling through every raster.
                let Some((oldest, _)) = self.entries
                                            .iter()
                                            .enumerate()
                                            .filter(|(_, entry)| {
                                                entry.valid_len == 0
                                                || self.frame.wrapping_sub(entry.frame) > 1
                                            })
                                            .min_by_key(|(_, entry)| entry.touched)
                else
                {
                    return;
                };
                self.entries.swap_remove(oldest);
            }
            let mut pixels = Vec::new();
            if self.entries.try_reserve(1).is_err() || pixels.try_reserve_exact(count).is_err()
            {
                return;
            }
            pixels.resize(count, 0);
            // The allocator may provide more capacity than requested.
            if self.bytes() + pixels.capacity() * 4 > PIXEL_BUDGET
            {
                return;
            }
            self.entries.push(Entry { id,
                                      bounds,
                                      origin: canvas.origin(),
                                      screen: (canvas.width, canvas.height),
                                      crop,
                                      pixels,
                                      valid: [Rect::ZERO; MAX_FRAGMENTS],
                                      valid_len: 0,
                                      touched: self.clock,
                                      frame: self.frame });
            self.entries.len() - 1
        };
        self.entries[index].touched = self.clock;
        self.entries[index].frame = self.frame;
        self.entries[index].copy(canvas, visible, true);
    }
}

#[cfg(test)]
mod tests
{
    use alloc::vec;
    use core::cell::Cell;

    use super::*;

    #[test]
    fn actual_surface_phase_fragments_and_commits_are_preserved()
    {
        let mut cache = ContentCache::default();
        let mut pixels = vec![0x777777; 64 * 48];
        let bounds = Rect::new(11, 13, 51, 43);
        let left = Rect::new(0, 0, 20, 30);
        let right = Rect::new(20, 0, 40, 30);
        let replays = Cell::new(0);
        let pattern = [0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01];
        let paint = |canvas: &mut Canvas<'_>| {
            replays.set(replays.get() + 1);
            canvas.fill_rect(Rect::new(0, 0, 40, 30), pattern, 0x123456, 0xfedcba);
        };
        {
            let mut canvas = Canvas::new(&mut pixels, 64, 48);
            canvas.set_origin(11, 13);
            canvas.set_clip(left);
            cache.paint(1, bounds, &mut canvas, paint);
            canvas.set_clip(right);
            cache.paint(1, bounds, &mut canvas, paint);
        }
        let original = pixels.clone();
        pixels.fill(0x777777);
        {
            let mut canvas = Canvas::new(&mut pixels, 64, 48);
            canvas.set_origin(11, 13);
            canvas.set_clip(right);
            cache.paint(1, bounds, &mut canvas, paint);
            canvas.set_clip(left);
            cache.paint(1, bounds, &mut canvas, paint);
        }
        assert_eq!(replays.get(), 2);
        assert_eq!(pixels, original);
        for y in 13..43
        {
            for x in 11..51
            {
                assert_eq!(pixels[y * 64 + x],
                           if (x & 7) == (y & 7) { 0x123456 } else { 0xfedcba });
            }
        }
        cache.invalidate(1);
        let mut canvas = Canvas::new(&mut pixels, 64, 48);
        canvas.set_origin(11, 13);
        canvas.set_clip(Rect::new(0, 0, 40, 30));
        cache.paint(1, bounds, &mut canvas, |canvas| canvas.paint_rect(left, 0));
        assert_eq!(pixels[20 * 64 + 20], 0);
        // Bounds changes invalidate even when dimensions remain identical: a
        // global pattern cannot be moved by copying the previous raster.
        let mut canvas = Canvas::new(&mut pixels, 64, 48);
        canvas.set_origin(12, 13);
        canvas.set_clip(Rect::new(0, 0, 40, 30));
        cache.paint(1, Rect::new(12, 13, 52, 43), &mut canvas, paint);
        assert_eq!(replays.get(), 3);
        cache.retain(|_| false);
        assert_eq!(cache.bytes(), 0);
    }

    #[test]
    fn pixel_storage_is_bounded_without_cycling_hot_background_rasters()
    {
        let mut cache = ContentCache::default();
        let mut pixels = vec![0; 640 * 480];
        let mut canvas = Canvas::new(&mut pixels, 640, 480);
        let bounds = Rect::new(0, 0, 640, 480);
        let replays = Cell::new(0);
        let paint = |canvas: &mut Canvas<'_>| {
            replays.set(replays.get() + 1);
            canvas.paint_rect(bounds, 0x123456);
        };
        for id in 1..=32
        {
            cache.paint(id, bounds, &mut canvas, paint);
            assert!(cache.bytes() <= PIXEL_BUDGET);
        }
        assert_eq!(cache.entries.len(), 1);
        cache.paint(1, bounds, &mut canvas, paint);
        assert_eq!(replays.get(), 32);
        cache.paint(32, bounds, &mut canvas, paint);
        assert_eq!(replays.get(), 33);
        cache.begin_frame();
        cache.begin_frame();
        cache.paint(32, bounds, &mut canvas, paint);
        assert_eq!(replays.get(), 34);
        cache.paint(32, bounds, &mut canvas, paint);
        assert_eq!(replays.get(), 34);
        cache.paint(1, bounds, &mut canvas, paint);
        assert_eq!(replays.get(), 35);
    }
}
