//! Bounded acceleration for committed retained window drawing.
//!
//! Populate from the real screen after the ordinary clipped replay. Rendering
//! into a small local surface changes global pattern phase and can change source
//! clipping, so this cache never substitutes a different rendering surface.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use super::{Canvas, Point, Rect, geometry::sect_rect, retained::SceneTarget};

const MAX_FRAGMENTS: usize = 64;
const MAX_WINDOWS: usize = 32;
pub const PIXEL_BUDGET: usize = 2 * 1024 * 1024;

struct Entry
{
    id:        SceneTarget,
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
        if save
        {
            self.mark_valid(rect);
        }
    }

    /// Repaired holes join their adjacent cached pieces again. Without this,
    /// a line crossing a repaired icon could miss despite every pixel being
    /// valid, making transient outlines replay text unnecessarily.
    fn mark_valid(&mut self, mut rect: Rect)
    {
        let mut index = 0;
        while index < self.valid_len
        {
            let old = self.valid[index];
            if old.left <= rect.left
               && old.top <= rect.top
               && old.right >= rect.right
               && old.bottom >= rect.bottom
            {
                return;
            }
            let contained = rect.left <= old.left
                            && rect.top <= old.top
                            && rect.right >= old.right
                            && rect.bottom >= old.bottom;
            let horizontal = old.top == rect.top
                             && old.bottom == rect.bottom
                             && old.left <= rect.right
                             && old.right >= rect.left;
            let vertical = old.left == rect.left
                           && old.right == rect.right
                           && old.top <= rect.bottom
                           && old.bottom >= rect.top;
            if contained || horizontal || vertical
            {
                rect = Rect::new(rect.left.min(old.left),
                                 rect.top.min(old.top),
                                 rect.right.max(old.right),
                                 rect.bottom.max(old.bottom));
                self.valid_len -= 1;
                self.valid[index] = self.valid[self.valid_len];
                index = 0;
            }
            else
            {
                index += 1;
            }
        }
        if self.valid_len < MAX_FRAGMENTS
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
    frame:   u64,
    budget:  Option<usize>
}

impl ContentCache
{
    /// Cache pixels are expendable. Reserve caller headroom before opportunistic
    /// allocation, and release old entries when other services consumed it.
    pub fn preserve_headroom(&mut self, available: usize, reserve: usize, maximum: usize)
    {
        let budget = self.bytes().saturating_add(available).saturating_sub(reserve).min(maximum);
        self.set_budget(budget);
    }

    pub fn set_budget(&mut self, bytes: usize)
    {
        self.budget = Some(bytes.min(PIXEL_BUDGET));
        while self.bytes() > bytes
        {
            let Some((index, _)) =
                self.entries.iter().enumerate().min_by_key(|(_, entry)| entry.touched)
            else
            {
                break;
            };
            self.entries.swap_remove(index);
        }
    }

    pub fn begin_frame(&mut self)
    {
        self.frame = self.frame.wrapping_add(1);
    }

    /// Keep allocated storage across commits; pending drawing cannot invalidate
    /// the old committed scene until EndUpdate publishes its replacement.
    pub fn invalidate(&mut self, id: impl Into<SceneTarget>)
    {
        let id = id.into();
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id)
        {
            entry.valid_len = 0;
        }
    }

    /// Preserve cached pixels outside a local content change. Fragment budget
    /// exhaustion drops validity, never correctness or the pixel allocation.
    pub fn invalidate_rect(&mut self, id: impl Into<SceneTarget>, mut damage: Rect)
    {
        let id = id.into();
        let Some(entry) = self.entries.iter_mut().find(|e| e.id == id)
        else
        {
            return;
        };
        damage.offset(entry.origin.h, entry.origin.v);
        let mut valid = [Rect::ZERO; MAX_FRAGMENTS];
        let mut length = 0;
        for &rect in &entry.valid[..entry.valid_len]
        {
            let mut hit = Rect::ZERO;
            let pieces = if sect_rect(rect, damage, &mut hit)
            {
                [Rect::new(rect.left, rect.top, rect.right, hit.top),
                 Rect::new(rect.left, hit.bottom, rect.right, rect.bottom),
                 Rect::new(rect.left, hit.top, hit.left, hit.bottom),
                 Rect::new(hit.right, hit.top, rect.right, hit.bottom)]
            }
            else
            {
                [rect, Rect::ZERO, Rect::ZERO, Rect::ZERO]
            };
            for piece in pieces.into_iter().filter(|r| !r.is_empty())
            {
                if length == MAX_FRAGMENTS
                {
                    entry.valid_len = 0;
                    return;
                }
                valid[length] = piece;
                length += 1;
            }
        }
        entry.valid = valid;
        entry.valid_len = length;
    }

    /// XOR cached content in place. Invalid pixels may be toggled too; they
    /// remain invalid and are replaced by normal retained replay before use.
    pub fn invert(&mut self, id: impl Into<SceneTarget>, local: Rect)
    {
        let id = id.into();
        let Some(entry) = self.entries.iter_mut().find(|e| e.id == id)
        else
        {
            return;
        };
        let mut screen = local;
        screen.offset(entry.origin.h, entry.origin.v);
        let mut rect = Rect::ZERO;
        if !sect_rect(screen, entry.crop, &mut rect)
        {
            return;
        }
        let stride = (entry.crop.right - entry.crop.left) as usize;
        for y in rect.top..rect.bottom
        {
            let start =
                (y - entry.crop.top) as usize * stride + (rect.left - entry.crop.left) as usize;
            for pixel in &mut entry.pixels[start..start + (rect.right - rect.left) as usize]
            {
                *pixel ^= 0x00ffffff;
            }
        }
    }

    pub fn retain(&mut self, mut live: impl FnMut(SceneTarget) -> bool)
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
    #[cfg(test)]
    pub fn paint(&mut self, id: impl Into<SceneTarget>, bounds: Rect, canvas: &mut Canvas<'_>,
                 replay: impl FnOnce(&mut Canvas<'_>))
    {
        let result: Result<(), core::convert::Infallible> =
            self.try_paint(id, bounds, canvas, |canvas| {
                    replay(canvas);
                    Ok(())
                });
        match result
        {
            Ok(()) => (),
            Err(never) => match never {}
        }
    }

    /// Refused resource context must not publish partial pixels as a valid
    /// cache entry. The compositor decides whether to present the frame.
    pub fn try_paint<E>(&mut self, id: impl Into<SceneTarget>, bounds: Rect,
                        canvas: &mut Canvas<'_>,
                        replay: impl FnOnce(&mut Canvas<'_>) -> Result<(), E>)
                        -> Result<(), E>
    {
        let id = id.into();
        let budget = self.budget.unwrap_or(PIXEL_BUDGET);
        let mut crop = Rect::ZERO;
        let mut visible = Rect::ZERO;
        if !sect_rect(bounds, Rect::new(0, 0, canvas.width as i16, canvas.height as i16), &mut crop)
           || !sect_rect(crop, canvas.clip(), &mut visible)
        {
            return Ok(());
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
                return Ok(());
            }
        }

        replay(canvas)?;

        let index = if let Some(index) = self.entries.iter().position(|entry| entry.id == id)
        {
            index
        }
        else
        {
            let count = (crop.right - crop.left) as usize * (crop.bottom - crop.top) as usize;
            let bytes = count * core::mem::size_of::<u32>();
            if bytes > budget
            {
                return Ok(());
            }
            while self.entries.len() >= MAX_WINDOWS || self.bytes() + bytes > budget
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
                    return Ok(());
                };
                self.entries.swap_remove(oldest);
            }
            let mut pixels = Vec::new();
            if self.entries.try_reserve(1).is_err() || pixels.try_reserve_exact(count).is_err()
            {
                return Ok(());
            }
            pixels.resize(count, 0);
            // The allocator may provide more capacity than requested.
            if self.bytes() + pixels.capacity() * 4 > budget
            {
                return Ok(());
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
        Ok(())
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

    #[test]
    fn local_invalidation_keeps_other_pixels_warm_at_a_nonzero_origin()
    {
        let mut cache = ContentCache::default();
        let mut pixels = vec![0; 64 * 48];
        let mut canvas = Canvas::new(&mut pixels, 64, 48);
        canvas.set_origin(11, 13);
        canvas.set_clip(Rect::new(0, 0, 40, 30));
        let bounds = Rect::new(11, 13, 51, 43);
        let paints = Cell::new(0);
        let paint = |c: &mut Canvas<'_>| {
            paints.set(paints.get() + 1);
            c.paint_rect(Rect::new(0, 0, 40, 30), 0x123456);
        };
        cache.paint(1, bounds, &mut canvas, paint);
        cache.invalidate_rect(1, Rect::new(2, 2, 6, 6));
        canvas.set_clip(Rect::new(20, 10, 30, 20));
        cache.paint(1, bounds, &mut canvas, paint);
        assert_eq!(paints.get(), 1, "unrelated content lost its cached pixels");
        canvas.set_clip(Rect::new(2, 2, 6, 6));
        cache.paint(1, bounds, &mut canvas, paint);
        assert_eq!(paints.get(), 2);
        canvas.set_clip(Rect::new(20, 10, 30, 20));
        cache.paint(1, bounds, &mut canvas, paint);
        assert_eq!(paints.get(), 2);
    }
}

#[cfg(test)]
mod repaired_hole_tests
{
    use super::*;
    #[test]
    fn repaired_item_holes_restore_a_single_warm_raster_for_crossing_outlines()
    {
        let mut pixels = alloc::vec![0;200*100];
        let mut canvas = Canvas::new(&mut pixels, 200, 100);
        let bounds = Rect::new(0, 0, 200, 100);
        let mut cache = ContentCache::default();
        cache.paint(1, bounds, &mut canvas, |canvas| canvas.paint_rect(bounds, 0x123456));
        for iteration in 0..100
        {
            let hole = Rect::new(40, 20, 80, 65);
            cache.invalidate_rect(1, hole);
            canvas.set_clip(hole);
            cache.paint(1, bounds, &mut canvas, |canvas| canvas.paint_rect(bounds, iteration));
            canvas.set_clip(Rect::new(0, 35, 200, 36));
            cache.paint(1, bounds, &mut canvas, |_| {
                     panic!("crossing line must use repaired cache")
                 });
            assert_eq!(cache.entries[0].valid_len, 1);
        }
    }
}

#[cfg(test)]
mod headroom_tests
{
    use super::*;
    #[test]
    fn pixel_cache_yields_to_other_services_without_changing_rendered_results()
    {
        let mut pixels = alloc::vec![0;200*100];
        let mut canvas = Canvas::new(&mut pixels, 200, 100);
        let bounds = Rect::new(0, 0, 200, 100);
        let mut cache = ContentCache::default();
        for id in [1, 2]
        {
            cache.paint(id, bounds, &mut canvas, |canvas| {
                     canvas.paint_rect(bounds, u32::try_from(id).unwrap())
                 });
        }
        assert_eq!(cache.bytes(), 160_000);
        cache.preserve_headroom(64_000, 128_000, PIXEL_BUDGET);
        assert_eq!(cache.bytes(), 80_000);
        cache.preserve_headroom(0, 128_000, PIXEL_BUDGET);
        assert_eq!(cache.bytes(), 0);
        cache.paint(3, bounds, &mut canvas, |canvas| canvas.paint_rect(bounds, 0x123456));
        assert!(canvas.pixels.iter().all(|pixel| *pixel == 0x123456));
        assert_eq!(cache.bytes(), 0, "replay may not consume reserved headroom");
    }
}

#[cfg(test)]
mod fallible_replay_tests
{
    use super::*;
    #[test]
    fn failed_resource_replay_never_marks_partial_pixels_valid()
    {
        let bounds = Rect::new(0, 0, 20, 20);
        let mut pixels = alloc::vec![0; 400];
        let mut canvas = Canvas::new(&mut pixels, 20, 20);
        let mut cache = ContentCache::default();
        assert_eq!(cache.try_paint(1, bounds, &mut canvas, |canvas| {
                            canvas.paint_rect(bounds, 0xff0000);
                            Err(mac_abi::NOT_FOUND)
                        }),
                   Err(mac_abi::NOT_FOUND));
        assert_eq!(cache.bytes(), 0);
        let mut replayed = false;
        assert_eq!(cache.try_paint(1, bounds, &mut canvas, |canvas| {
                            replayed = true;
                            canvas.paint_rect(bounds, 0x123456);
                            Ok::<(), i32>(())
                        }),
                   Ok(()));
        assert!(replayed, "a failed frame was cached as valid");
        canvas.pixels.fill(0);
        assert_eq!(cache.try_paint(1, bounds, &mut canvas, |_| Err(mac_abi::NOT_FOUND)), Ok(()));
        assert!(canvas.pixels.iter().all(|pixel| *pixel == 0x123456));
    }
    #[test]
    fn failed_hole_replay_keeps_it_invalid_and_no_cache_budget_preserves_error()
    {
        let bounds = Rect::new(0, 0, 20, 20);
        let hole = Rect::new(2, 3, 7, 8);
        let mut pixels = alloc::vec![0; 400];
        let mut canvas = Canvas::new(&mut pixels, 20, 20);
        let mut cache = ContentCache::default();
        cache.paint(1, bounds, &mut canvas, |canvas| canvas.paint_rect(bounds, 9));
        cache.invalidate_rect(1, hole);
        canvas.set_clip(hole);
        assert_eq!(cache.try_paint(1, bounds, &mut canvas, |_| Err(mac_abi::NOT_FOUND)),
                   Err(mac_abi::NOT_FOUND));
        let mut repaired = false;
        cache.try_paint(1, bounds, &mut canvas, |canvas| {
                 repaired = true;
                 canvas.paint_rect(hole, 7);
                 Ok::<(), i32>(())
             })
             .unwrap();
        assert!(repaired);
        cache.set_budget(0);
        assert_eq!(cache.try_paint(2, bounds, &mut canvas, |_| Err(mac_abi::NOT_FOUND)),
                   Err(mac_abi::NOT_FOUND));
        assert_eq!(cache.bytes(), 0);
    }
}
