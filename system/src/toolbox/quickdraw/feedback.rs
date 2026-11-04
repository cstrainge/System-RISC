//! Present-time drag feedback and cached, scene-owned visibility clips.
#![forbid(unsafe_code)]
use alloc::{sync::Arc, vec::Vec};

use mac_abi::{BAD_ARGUMENT, NO_MEMORY, SERVICE_BUSY, Status};

use super::{Canvas, Point, Rect,
            geometry::{outline_edges, sect_rect},
            presentation::{Outline, indexed_tracking_xor}};
#[cfg(systemrisc)]
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

    #[cfg(not(systemrisc))]
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
#[cfg(systemrisc)]
pub(crate) struct VisibilityCache
{
    target: Option<Option<WindowId>>,
    clip:   Clip
}
#[cfg(systemrisc)]
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
        // This cache and its immutable snapshots belong to the shared scene,
        // and can outlive the app whose drag first needed them. Admit their
        // original backing as ordinary System storage, never an app receipt
        // or a recovery dialog's protected allocation.
        use mac_object_table::shared::InvocationGate as _;
        self.clip = crate::context::without_recovery(|| {
            crate::toolbox::handles::InvocationGate::with_system_allocation(|| {
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
                                                            crate::display::width() as i16,
                                                            crate::display::height() as i16),
                                                  &mut collect);
                }
                if exhausted
                {
                    allocation_refused(target);
                }
                if exhausted || rects.is_empty()
                {
                    Clip::Empty
                }
                else
                {
                    Arc::try_new(rects).map(Clip::Visible).unwrap_or_else(|_| {
                                                              allocation_refused(target);
                                                              Clip::Empty
                                                          })
                }
            })
        });
        // Negative results are cached too. Allocation refusal suppresses this
        // feedback until a real scene/target change, never a per-input retry loop.
        self.target = Some(target);
        self.clip.clone()
    }
}

#[cfg(systemrisc)]
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

/// A cropped, definition-produced white mask. Its original allocation receipt
/// remains live through the last frontbuffer erase. Padding is never painted.
#[derive(Debug)]
pub(crate) struct GrowMask
{
    owner:  mac_object_table::domain::DomainId,
    bounds: Rect,
    clip:   Rect,
    pixels: Vec<u32>
}
impl GrowMask
{
    pub(crate) fn prepare(reuse: &mut Option<Arc<Self>>,
                          owner: mac_object_table::domain::DomainId, structure: Rect, screen: Rect)
                          -> Result<(), Status>
    {
        let mut clip = Rect::ZERO;
        if !sect_rect(structure, screen, &mut clip) || clip.top < 0 || clip.left < 0
        {
            return Err(BAD_ARGUMENT);
        }
        clip.top = clip.top.max(21);
        if clip.is_empty()
        {
            return Err(BAD_ARGUMENT);
        }
        // Preserve the author's global eight-pixel pattern phase.
        let bounds = Rect::new(clip.left & !7, clip.top & !7, clip.right, clip.bottom);
        let width = usize::try_from(bounds.right - bounds.left).map_err(|_| BAD_ARGUMENT)?;
        let height = usize::try_from(bounds.bottom - bounds.top).map_err(|_| BAD_ARGUMENT)?;
        let count = width.checked_mul(height).ok_or(NO_MEMORY)?;
        if reuse.is_none()
        {
            *reuse = Some(Arc::try_new(Self { owner,
                                              bounds,
                                              clip,
                                              pixels: Vec::new() }).map_err(|_| NO_MEMORY)?);
        }
        let mask = Arc::get_mut(reuse.as_mut().ok_or(SERVICE_BUSY)?).ok_or(SERVICE_BUSY)?;
        if mask.owner != owner
        {
            return Err(BAD_ARGUMENT);
        }
        mask.pixels
            .try_reserve_exact(count.saturating_sub(mask.pixels.len()))
            .map_err(|_| NO_MEMORY)?;
        mask.pixels.truncate(count);
        mask.pixels.fill(super::WHITE);
        mask.pixels.resize(count, super::WHITE);
        mask.bounds = bounds;
        mask.clip = clip;
        Ok(())
    }

    pub(crate) fn storage_owner(&self) -> mac_object_table::domain::DomainId
    {
        self.owner
    }

    pub(crate) fn canvas(&mut self) -> Canvas<'_>
    {
        let mut canvas = Canvas::new(&mut self.pixels,
                                     (self.bounds.right - self.bounds.left) as usize,
                                     (self.bounds.bottom - self.bounds.top) as usize);
        canvas.set_origin(-self.bounds.left, -self.bounds.top);
        canvas.set_clip(self.clip);
        canvas
    }

    fn contains(&self, x: usize, y: usize) -> bool
    {
        if (x + y) & 1 != 1
           || x > i16::MAX as usize
           || y > i16::MAX as usize
           || !self.clip.contains(Point { h: x as i16,
                                          v: y as i16 })
        {
            return false;
        }
        let width = (self.bounds.right - self.bounds.left) as usize;
        self.pixels[(y - self.bounds.top as usize) * width + x - self.bounds.left as usize]
        != super::WHITE
    }

    fn visit(&self, rect: Rect, mut pixel: impl FnMut(usize, usize))
    {
        let mut clip = Rect::ZERO;
        if !sect_rect(rect, self.clip, &mut clip)
        {
            return;
        }
        for y in clip.top as usize..clip.bottom as usize
        {
            for x in clip.left as usize..clip.right as usize
            {
                if self.contains(x, y)
                {
                    pixel(x, y);
                }
            }
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct GrowOverlay(pub(crate) Arc<GrowMask>);
impl PartialEq for GrowOverlay
{
    fn eq(&self, other: &Self) -> bool
    {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for GrowOverlay
{
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub(crate) struct Feedback
{
    pub grow:      Option<GrowOverlay>,
    pub outline:   Option<ClippedOutline>,
    pub highlight: Option<Highlight>
}
impl Feedback
{
    pub(crate) fn outline(shape: Outline, clip: Clip) -> Self
    {
        Self { grow:      None,
               outline:   Some(ClippedOutline { shape,
                                                clip }),
               highlight: None }
    }

    fn color(&self, rgb: u32, x: usize, y: usize) -> u32
    {
        let rgb = self.highlight.as_ref().map_or(rgb, |highlight| highlight.color(rgb, x, y));
        let rgb = if self.grow.as_ref().is_some_and(|mask| mask.0.contains(x, y))
        {
            indexed_tracking_xor(rgb)
        }
        else
        {
            rgb
        };
        self.outline.as_ref().map_or(rgb, |outline| outline.color(rgb, x, y))
    }

    fn visit(&self, rect: Rect, mut pixel: impl FnMut(usize, usize))
    {
        if let Some(highlight) = &self.highlight
        {
            highlight.visit(rect, &mut pixel);
        }
        if let Some(mask) = &self.grow
        {
            mask.0.visit(rect, &mut pixel);
        }
        if let Some(outline) = &self.outline
        {
            outline.visit(rect, &mut pixel);
        }
    }

    pub(crate) fn paint(&self, scene: &[u32], front: &mut [u32], width: usize, rect: Rect)
    {
        self.visit(rect, |x, y| {
                let at = y * width + x;
                front[at] = self.color(scene[at], x, y);
            });
    }

    pub(crate) fn transition(&self, previous: &Self, scene: &[u32], front: &mut [u32],
                             width: usize, rect: Rect)
                             -> usize
    {
        if self == previous
        {
            return 0;
        }
        let mut changed = 0;
        // Visit only each changed object's actual pixels. Repeated overlap is
        // idempotent: compute from clean scene, never XOR a pixel twice.
        let mut pixel = |x, y| {
            let at = y * width + x;
            let next = self.color(scene[at], x, y);
            if front[at] != next
            {
                front[at] = next;
                changed += 1;
            }
        };
        for value in [previous, self]
        {
            if previous.highlight != self.highlight
            {
                if let Some(highlight) = &value.highlight
                {
                    highlight.visit(rect, &mut pixel);
                }
            }
            if previous.grow != self.grow
            {
                if let Some(mask) = &value.grow
                {
                    mask.0.visit(rect, &mut pixel);
                }
            }
            if previous.outline != self.outline
            {
                if let Some(outline) = &value.outline
                {
                    outline.visit(rect, &mut pixel);
                }
            }
        }
        changed
    }
}
