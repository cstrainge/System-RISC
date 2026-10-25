//! Present-time drag feedback and cached, scene-owned visibility clips.
#![forbid(unsafe_code)]
use alloc::{sync::Arc, vec::Vec};

use super::{Point, Rect,
            geometry::{outline_edges, sect_rect},
            presentation::{Outline, indexed_tracking_xor}};
#[cfg(target_arch = "riscv64")]
use crate::toolbox::windows::{WindowId, WindowManager};

#[derive(Clone, Default, Debug)]
pub(crate) enum Clip
{
    #[default]
    Empty,
    Screen,
    // Only the Window Manager's disjoint visibility sweep publishes this list.
    Visible(Arc<Vec<Rect>>)
}
impl PartialEq for Clip
{
    fn eq(&self, other: &Self) -> bool
    {
        match (self, other)
        {
            (Self::Empty, Self::Empty) | (Self::Screen, Self::Screen) => true,
            (Self::Visible(a), Self::Visible(b)) => Arc::ptr_eq(a, b),
            _ => false
        }
    }
}
impl Eq for Clip
{
}
impl Clip
{
    fn visit(&self, rect: Rect, mut visit: impl FnMut(Rect))
    {
        let rect = Rect::new(rect.left.max(0), rect.top.max(21), rect.right, rect.bottom);
        if rect.is_empty()
        {
            return;
        }
        match self
        {
            Self::Empty => (),
            Self::Screen => visit(rect),
            Self::Visible(rects) =>
            {
                for &visible in rects.iter()
                {
                    let mut overlap = Rect::ZERO;
                    if sect_rect(rect, visible, &mut overlap)
                    {
                        visit(overlap);
                    }
                }
            }
        }
    }

    fn contains(&self, x: usize, y: usize) -> bool
    {
        if y < 21
        {
            return false;
        }
        match self
        {
            Self::Empty => false,
            Self::Screen => true,
            Self::Visible(rects) =>
            {
                rects.iter().any(|r| {
                                r.contains(Point { h: x as i16,
                                                   v: y as i16 })
                            })
            }
        }
    }

    #[cfg(not(target_arch = "riscv64"))]
    pub(crate) fn rectangles(rectangles: &[Rect], previous: Option<&Self>) -> Option<Self>
    {
        if let Some(Self::Visible(old)) = previous
        {
            if old.as_slice() == rectangles
            {
                return previous.cloned();
            }
        }
        // Trusted capture geometry must respect the same disjoint-fragment
        // contract as the Window Manager; reject overlaps before publication.
        for (i, &rect) in rectangles.iter().enumerate()
        {
            for &other in &rectangles[..i]
            {
                let mut overlap = Rect::ZERO;
                if sect_rect(rect, other, &mut overlap)
                {
                    return None;
                }
            }
        }
        let mut rects = Vec::new();
        rects.try_reserve_exact(rectangles.len()).ok()?;
        rects.extend_from_slice(rectangles);
        Some(Self::Visible(Arc::try_new(rects).ok()?))
    }
}

/// A visibility sweep happens only when its target or committed scene geometry
/// changes. Arc snapshots keep old clipping alive until its pixels are erased.
#[derive(Default)]
#[cfg(target_arch = "riscv64")]
pub(crate) struct VisibilityCache
{
    target: Option<Option<WindowId>>,
    clip:   Clip
}
#[cfg(target_arch = "riscv64")]
impl VisibilityCache
{
    pub(crate) fn invalidate(&mut self)
    {
        self.target = None;
    }

    pub(crate) fn get(&mut self, windows: &WindowManager, target: Option<WindowId>) -> Clip
    {
        if self.target == Some(target)
        {
            return self.clip.clone();
        }
        let mut rects = Vec::new();
        let mut exhausted = false;
        let mut collect = |rect: Rect| {
            if exhausted || rect.is_empty()
            {
                return;
            }
            if rects.try_reserve(1).is_err()
            {
                exhausted = true;
                return;
            }
            rects.push(rect);
        };
        if let Some(id) = target
        {
            windows.for_each_visible_rect(id, &mut collect);
        }
        else
        {
            windows.for_each_desktop_rect(Rect::new(0,
                                                    21,
                                                    crate::display::WIDTH as i16,
                                                    crate::display::HEIGHT as i16),
                                          &mut collect);
        }
        if exhausted
        {
            allocation_refused(target);
        }
        self.clip = if exhausted || rects.is_empty()
        {
            Clip::Empty
        }
        else
        {
            Arc::try_new(rects).map(Clip::Visible).unwrap_or_else(|_| {
                                                      allocation_refused(target);
                                                      Clip::Empty
                                                  })
        };
        // Negative results are cached too. Allocation refusal suppresses this
        // feedback until a real scene/target change, never a per-input retry loop.
        self.target = Some(target);
        self.clip.clone()
    }
}

#[cfg(target_arch = "riscv64")]
fn allocation_refused(target: Option<WindowId>)
{
    serial!("FEEDBACK_CLIP_UNAVAILABLE: target={target:?}; not enough memory for visibility \
             snapshot; feedback suppressed until scene or target changes");
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ClippedOutline
{
    pub shape: Outline,
    pub clip:  Clip
}
impl ClippedOutline
{
    fn contains(&self, x: usize, y: usize) -> bool
    {
        self.shape.contains(x, y) && self.clip.contains(x, y)
    }

    fn visit(&self, rect: Rect, mut pixel: impl FnMut(usize, usize))
    {
        self.clip.visit(rect, |clip| self.shape.visit(clip, &mut pixel));
    }

    fn difference(&self, other: Option<&Self>, rect: Rect, mut pixel: impl FnMut(usize, usize))
    {
        if other == Some(self)
        {
            return;
        }
        if let Some(other) = other.filter(|other| other.clip == self.clip)
        {
            self.clip.visit(rect, |clip| {
                         self.shape.visit_difference(Some(other.shape), clip, &mut pixel)
                     });
        }
        else
        {
            self.visit(rect, |x, y| {
                    if !other.is_some_and(|other| other.contains(x, y))
                    {
                        pixel(x, y);
                    }
                });
        }
    }

    fn color(&self, rgb: u32, x: usize, y: usize) -> u32
    {
        if self.contains(x, y) { indexed_tracking_xor(rgb) } else { rgb }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Highlight
{
    pub bounds: Rect,
    pub clip:   Clip
}
impl Highlight
{
    fn contains(&self, x: usize, y: usize) -> bool
    {
        let (x, y) = (x as i32, y as i32);
        let [l, t, r, b] = [i32::from(self.bounds.left),
                            i32::from(self.bounds.top),
                            i32::from(self.bounds.right),
                            i32::from(self.bounds.bottom)];
        x >= l
        && x < r
        && y >= t
        && y < b
        && (x < l + 2 || x >= r - 2 || y < t + 2 || y >= b - 2)
        && self.clip.contains(x as usize, y as usize)
    }

    fn visit(&self, rect: Rect, mut pixel: impl FnMut(usize, usize))
    {
        self.clip.visit(rect, |clip| {
                     for inset in [0, 1]
                     {
                         let mut bounds = self.bounds;
                         bounds.inset(inset, inset);
                         // Each perimeter is disjoint; guard the one-row/column cases
                         // where the classic four-edge helper repeats opposing sides.
                         if bounds.is_empty()
                         {
                             continue;
                         }
                         let edges = outline_edges(bounds);
                         for (i, edge) in edges.into_iter().enumerate()
                         {
                             if i == 1 && i32::from(bounds.bottom) - i32::from(bounds.top) == 1
                             {
                                 continue;
                             }
                             if i == 3 && i32::from(bounds.right) - i32::from(bounds.left) == 1
                             {
                                 continue;
                             }
                             let mut overlap = Rect::ZERO;
                             if sect_rect(edge, clip, &mut overlap)
                             {
                                 for y in overlap.top..overlap.bottom
                                 {
                                     for x in overlap.left..overlap.right
                                     {
                                         pixel(x as usize, y as usize);
                                     }
                                 }
                             }
                         }
                     }
                 });
    }

    fn color(&self, rgb: u32, x: usize, y: usize) -> u32
    {
        // Preserve the existing drop-target color; this is presentation state,
        // never a temporary write into the retained scene.
        if self.contains(x, y) { 0x2222aa } else { rgb }
    }
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub(crate) struct Feedback
{
    pub outline:   Option<ClippedOutline>,
    pub highlight: Option<Highlight>
}
impl Feedback
{
    pub(crate) fn outline(shape: Outline, clip: Clip) -> Self
    {
        Self { outline:   Some(ClippedOutline { shape,
                                                clip }),
               highlight: None }
    }

    fn base(&self, rgb: u32, x: usize, y: usize) -> u32
    {
        self.highlight.as_ref().map_or(rgb, |highlight| highlight.color(rgb, x, y))
    }

    pub(crate) fn paint(&self, scene: &[u32], front: &mut [u32], width: usize, rect: Rect)
    {
        if let Some(highlight) = &self.highlight
        {
            highlight.visit(rect, |x, y| {
                         front[y * width + x] = highlight.color(scene[y * width + x], x, y)
                     });
        }
        if let Some(outline) = &self.outline
        {
            outline.visit(rect, |x, y| {
                       front[y * width + x] =
                           indexed_tracking_xor(self.base(scene[y * width + x], x, y))
                   });
        }
    }

    pub(crate) fn transition(&self, previous: &Self, scene: &[u32], front: &mut [u32],
                             width: usize, rect: Rect)
                             -> usize
    {
        let mut changed = 0;
        if previous.highlight != self.highlight
        {
            // Change the lower layer while preserving the old upper outline.
            // Then the ordinary outline delta runs against the new lower layer.
            for (highlight, other) in [(previous.highlight.as_ref(), self.highlight.as_ref()),
                                       (self.highlight.as_ref(), previous.highlight.as_ref())]
            {
                if let Some(highlight) = highlight
                {
                    highlight.visit(rect, |x, y| {
                                 if other.is_some_and(|other| other.contains(x, y))
                                 {
                                     return;
                                 }
                                 let at = y * width + x;
                                 let old = previous.base(scene[at], x, y);
                                 let new = self.base(scene[at], x, y);
                                 let (old, new) =
                                     previous.outline.as_ref().map_or((old, new), |outline| {
                                                                  (outline.color(old, x, y),
                                                                   outline.color(new, x, y))
                                                              });
                                 front[at] ^= old ^ new;
                                 changed += 1;
                             });
                }
            }
        }
        if previous.outline != self.outline
        {
            let mut last = None;
            for (outline, other) in [(previous.outline.as_ref(), self.outline.as_ref()),
                                     (self.outline.as_ref(), previous.outline.as_ref())]
            {
                if let Some(outline) = outline
                {
                    outline.difference(other, rect, |x, y| {
                               let at = y * width + x;
                               let rgb = self.base(scene[at], x, y);
                               let mask = match last
                               {
                                   Some((color, mask)) if color == rgb => mask,
                                   _ =>
                                   {
                                       let mask = rgb ^ indexed_tracking_xor(rgb);
                                       last = Some((rgb, mask));
                                       mask
                                   }
                               };
                               front[at] ^= mask;
                               changed += 1;
                           });
                }
            }
        }
        changed
    }
}
