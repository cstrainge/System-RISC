//! Reusable visibility sweep storage, prepared on window growth, never on paint.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{NO_MEMORY, geometry::Rect, Status};

use crate::toolbox::quickdraw::geometry::sect_rect;

#[derive(Default)]
pub(super) struct Scratch
{
    rectangles: Vec<Rect>,
    edges:      Vec<i16>,
    intervals:  Vec<(i16, i16)>,
    previous:   Vec<Rect>,
    gaps:       Vec<Rect>
}
/// Empty replacement arrays only; paint never allocates or keeps these owners.
pub(super) struct CapacityPlan
{
    lengths: [usize; 5]
}
pub(super) struct PreparedCapacity(Scratch);
fn lengths(windows: usize) -> Result<[usize; 5], Status>
{
    let rectangles = windows.checked_mul(2).ok_or(NO_MEMORY)?;
    let edges = rectangles.checked_mul(2).and_then(|n| n.checked_add(2)).ok_or(NO_MEMORY)?;
    let gaps = rectangles.checked_add(1).ok_or(NO_MEMORY)?;
    Ok([rectangles, edges, rectangles, gaps, gaps])
}
fn detached<T: Clone>(length: usize, zero: T) -> Result<Vec<T>, Status>
{
    let mut values = Vec::new();
    values.try_reserve_exact(length).map_err(|_| NO_MEMORY)?;
    values.resize(length, zero);
    Ok(values)
}
impl CapacityPlan
{
    pub(super) fn prepare(self) -> Result<PreparedCapacity, Status>
    {
        Ok(PreparedCapacity(Scratch { rectangles: detached(self.lengths[0], Rect::ZERO)?,
                                      edges:      detached(self.lengths[1], 0)?,
                                      intervals:  detached(self.lengths[2], (0, 0))?,
                                      previous:   detached(self.lengths[3], Rect::ZERO)?,
                                      gaps:       detached(self.lengths[4], Rect::ZERO)? }))
    }
}
fn grow<T: Clone>(values: &mut Vec<T>, length: usize, empty: T) -> Result<(), Status>
{
    if length > values.len()
    {
        values.try_reserve(length - values.len()).map_err(|_| NO_MEMORY)?;
        values.resize(length, empty);
    }
    Ok(())
}
impl Scratch
{
    pub(super) fn plan_capacity(&self, windows: usize) -> Result<CapacityPlan, Status>
    {
        let required = lengths(windows)?;
        let current = [self.rectangles.capacity(),
                       self.edges.capacity(),
                       self.intervals.capacity(),
                       self.previous.capacity(),
                       self.gaps.capacity()];
        Ok(CapacityPlan { lengths: core::array::from_fn(|i| {
                              if current[i] < required[i] { required[i] } else { 0 }
                          }) })
    }

    pub(super) fn can_install(&self, windows: usize, prepared: &PreparedCapacity)
                              -> Result<bool, Status>
    {
        let required = lengths(windows)?;
        let current = [self.rectangles.capacity(),
                       self.edges.capacity(),
                       self.intervals.capacity(),
                       self.previous.capacity(),
                       self.gaps.capacity()];
        let replacement = [prepared.0.rectangles.capacity(),
                           prepared.0.edges.capacity(),
                           prepared.0.intervals.capacity(),
                           prepared.0.previous.capacity(),
                           prepared.0.gaps.capacity()];
        Ok((0..5).all(|i| current[i] >= required[i] || replacement[i] >= required[i]))
    }

    /// Check all geometry before mutation. Displaced buffers stay in prepared.
    pub(super) fn install_capacity(&mut self, windows: usize, prepared: &mut PreparedCapacity)
                                   -> Result<(), Status>
    {
        if !self.can_install(windows, prepared)?
        {
            return Err(mac_abi::SERVICE_BUSY);
        }
        let required = lengths(windows)?;
        fn install<T: Clone>(current: &mut Vec<T>, replacement: &mut Vec<T>, length: usize, zero: T)
        {
            if current.capacity() < length
            {
                core::mem::swap(current, replacement);
            }
            if current.len() < length
            {
                // Capacity was checked before any buffer moved.
                current.resize(length, zero);
            }
        }
        install(&mut self.rectangles, &mut prepared.0.rectangles, required[0], Rect::ZERO);
        install(&mut self.edges, &mut prepared.0.edges, required[1], 0);
        install(&mut self.intervals, &mut prepared.0.intervals, required[2], (0, 0));
        install(&mut self.previous, &mut prepared.0.previous, required[3], Rect::ZERO);
        install(&mut self.gaps, &mut prepared.0.gaps, required[4], Rect::ZERO);
        Ok(())
    }

    pub(super) fn prepare(&mut self, windows: usize) -> Result<(), Status>
    {
        let rectangles = windows.checked_mul(2).ok_or(NO_MEMORY)?;
        let edges = rectangles.checked_mul(2).and_then(|n| n.checked_add(2)).ok_or(NO_MEMORY)?;
        let gaps = rectangles.checked_add(1).ok_or(NO_MEMORY)?;
        grow(&mut self.rectangles, rectangles, Rect::ZERO)?;
        grow(&mut self.edges, edges, 0)?;
        grow(&mut self.intervals, rectangles, (0, 0))?;
        grow(&mut self.previous, gaps, Rect::ZERO)?;
        grow(&mut self.gaps, gaps, Rect::ZERO)
    }

    pub(super) fn visit(&mut self, bounds: Rect, covers: impl Iterator<Item = Rect>,
                        mut visit: impl FnMut(Rect))
    {
        if bounds.is_empty()
        {
            return;
        }
        let rectangles = &mut self.rectangles;
        let mut count = 0;
        for cover in covers
        {
            let mut clipped = Rect::ZERO;
            if sect_rect(bounds, cover, &mut clipped)
            {
                rectangles[count] = clipped;
                count += 1;
            }
        }
        if count == 0
        {
            visit(bounds);
            return;
        }
        let rectangles = &rectangles[..count];
        let edges = &mut self.edges;
        edges[0] = bounds.top;
        edges[1] = bounds.bottom;
        for (index, rect) in rectangles.iter().enumerate()
        {
            edges[2 + index * 2] = rect.top;
            edges[3 + index * 2] = rect.bottom;
        }
        let edges = &mut edges[..2 + count * 2];
        edges.sort_unstable();
        let intervals = &mut self.intervals;
        let previous = &mut self.previous;
        let mut previous_count = 0;
        for band in edges.windows(2).filter(|band| band[0] != band[1])
        {
            let mut covered = 0;
            for rect in rectangles
            {
                if rect.top <= band[0] && rect.bottom >= band[1]
                {
                    intervals[covered] = (rect.left, rect.right);
                    covered += 1;
                }
            }
            intervals[..covered].sort_unstable();
            let gaps = &mut self.gaps;
            let mut gap_count = 0;
            let mut left = bounds.left;
            for &(start, end) in &intervals[..covered]
            {
                if start > left
                {
                    gaps[gap_count] = Rect::new(left, band[0], start, band[1]);
                    gap_count += 1;
                }
                left = left.max(end);
            }
            if left < bounds.right
            {
                gaps[gap_count] = Rect::new(left, band[0], bounds.right, band[1]);
                gap_count += 1;
            }
            if gap_count == previous_count
               && gaps[..gap_count].iter().zip(&previous[..previous_count]).all(|(a, b)| {
                                                                               a.left == b.left
                                                                               && a.right == b.right
                                                                               && a.top == b.bottom
                                                                           })
            {
                for rect in &mut previous[..previous_count]
                {
                    rect.bottom = band[1];
                }
            }
            else
            {
                for rect in &previous[..previous_count]
                {
                    visit(*rect);
                }
                previous[..gap_count].copy_from_slice(&gaps[..gap_count]);
                previous_count = gap_count;
            }
        }
        for rect in &previous[..previous_count]
        {
            visit(*rect);
        }
    }
}

#[cfg(test)]
mod tests
{
    use mac_abi::Point;

    use super::*;
    #[test]
    fn large_occlusion_sets_reuse_scratch_and_growth_refusal_keeps_old_geometry_usable()
    {
        let covers: Vec<_> = (0..80).map(|i| {
                                        let x = (i % 10) * 10;
                                        let y = (i / 10) * 10;
                                        Rect::new(x, y, x + 3, y + 3)
                                    })
                                    .collect();
        let mut scratch = Scratch::default();
        scratch.prepare(covers.len()).unwrap();
        let storage = |scratch: &Scratch| {
            [(scratch.rectangles.as_ptr() as usize, scratch.rectangles.capacity()),
             (scratch.edges.as_ptr() as usize, scratch.edges.capacity()),
             (scratch.intervals.as_ptr() as usize, scratch.intervals.capacity()),
             (scratch.previous.as_ptr() as usize, scratch.previous.capacity()),
             (scratch.gaps.as_ptr() as usize, scratch.gaps.capacity())]
        };
        let prepared = storage(&scratch);
        for _ in 0..3
        {
            let mut pixels = [0u8; 100 * 100];
            scratch.visit(Rect::new(0, 0, 100, 100), covers.iter().copied(), |rect| {
                       for y in rect.top..rect.bottom
                       {
                           for x in rect.left..rect.right
                           {
                               pixels[y as usize * 100 + x as usize] += 1;
                           }
                       }
                   });
            for y in 0..100
            {
                for x in 0..100
                {
                    let covered = covers.iter().any(|rect| {
                                                   rect.contains(Point { h: x,
                                                                         v: y })
                                               });
                    assert_eq!(pixels[y as usize * 100 + x as usize],
                               u8::from(!covered),
                               "visibility changed at ({x},{y})");
                }
            }
            assert_eq!(storage(&scratch), prepared, "paint must never grow sweep storage");
            assert_eq!(scratch.prepare(usize::MAX), Err(NO_MEMORY));
            assert_eq!(storage(&scratch), prepared, "failed growth must retain the usable cache");
        }
    }
}
