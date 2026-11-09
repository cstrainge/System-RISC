//! Bounded acceleration for committed retained window drawing.
//!
//! Populate from the real screen after the ordinary clipped replay. Rendering
//! into a small local surface changes global pattern phase and can change source
//! clipping, so this cache never substitutes a different rendering surface.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_object_table::shared::InvocationGate as _;

use super::{Canvas, Point, Rect, geometry::sect_rect, retained::SceneTarget};
use crate::toolbox::handles::InvocationGate;

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
            // Retained compositor cache storage belongs to System. The replay
            // callback above keeps the original drawing subject's allocation.
            let admitted = InvocationGate::with_system_allocation(|| {
                self.entries.try_reserve(1)?;
                pixels.try_reserve_exact(count)
            });
            if admitted.is_err()
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


