//! Frontbuffer overlays. The retained scene is always read-only.
#![forbid(unsafe_code)]
use mac_abi::{Point, Rect, quickdraw::MonoCursor};

use super::feedback::Feedback;

/// Rectangle-union outline translated without changing retained geometry.
/// Visiting an outline costs its clipped perimeter, never its enclosed area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Outline
{
    pieces: [Rect; 2],
    dx:     i32,
    dy:     i32
}
impl Outline
{
    pub(crate) fn new(pieces: &[Rect], dx: i32, dy: i32) -> Option<Self>
    {
        if pieces.is_empty() || pieces.len() > 2
        {
            return None;
        }
        let mut stored = [Rect::ZERO; 2];
        stored[..pieces.len()].copy_from_slice(pieces);
        Some(Self { pieces: stored,
                    dx,
                    dy })
    }

    pub(super) fn contains(self, x: usize, y: usize) -> bool
    {
        if y < 21 || (x + y) & 1 != 1
        {
            return false;
        }
        let x = x as i64 - i64::from(self.dx);
        let y = y as i64 - i64::from(self.dy);
        let inside = |x, y| {
            self.pieces.iter().any(|r| {
                                  x >= i64::from(r.left)
                                  && x < i64::from(r.right)
                                  && y >= i64::from(r.top)
                                  && y < i64::from(r.bottom)
                              })
        };
        inside(x, y)
        && (!inside(x - 1, y) || !inside(x + 1, y) || !inside(x, y - 1) || !inside(x, y + 1))
    }

    /// Reject damage wholly inside/outside the structure before building spans.
    /// Most cursor boxes and text damage never touch a constituent perimeter.
    fn intersects_perimeter(self, clip: Rect) -> bool
    {
        let [l, t, r, b] = [i64::from(clip.left.max(0)),
                            i64::from(clip.top.max(21)),
                            i64::from(clip.right),
                            i64::from(clip.bottom)];
        self.pieces.iter().filter(|piece| !piece.is_empty()).any(|piece| {
                                                                let left = i64::from(piece.left)
                                                                           + i64::from(self.dx);
                                                                let right = i64::from(piece.right)
                                                                            + i64::from(self.dx);
                                                                let top = i64::from(piece.top)
                                                                          + i64::from(self.dy);
                                                                let bottom = i64::from(piece.bottom)
                                                                             + i64::from(self.dy);
                                                                (left < r
                                                                 && right > l
                                                                 && ((top >= t && top < b)
                                                                     || (bottom - 1 >= t
                                                                         && bottom - 1 < b)))
                                                                || (top < b
                                                                    && bottom > t
                                                                    && ((left >= l && left < r)
                                                                        || (right - 1 >= l
                                                                            && right - 1 < r)))
                                                            })
    }

    /// Eight rectangle sides, each split at most once by the other rectangle.
    /// Only outward-facing union edges survive; no per-pixel geometry tests.
    #[inline(never)]
    fn edges(self) -> [Option<Edge>; 16]
    {
        let rects = self.pieces.map(|r| {
                                   [i64::from(r.left) + i64::from(self.dx),
                                    i64::from(r.top) + i64::from(self.dy),
                                    i64::from(r.right) + i64::from(self.dx),
                                    i64::from(r.bottom) + i64::from(self.dy)]
                               });
        let mut edges = [None; 16];
        for (i, [left, top, right, bottom]) in rects.iter().copied().enumerate()
        {
            if left >= right || top >= bottom
            {
                continue;
            }
            let [l, t, r, b] = rects[1 - i];
            for (side, (horizontal, fixed, start, end, neighbor)) in
                [(true, top, left, right, top - 1),
                 (true, bottom - 1, left, right, bottom),
                 (false, left, top, bottom, left - 1),
                 (false, right - 1, top, bottom, right)].into_iter()
                                                        .enumerate()
            {
                let (cross_start, cross_end, along_start, along_end) =
                    if horizontal { (t, b, l, r) } else { (l, r, t, b) };
                let blocked = l < r && t < b && neighbor >= cross_start && neighbor < cross_end;
                let at = i * 8 + side * 2;
                if blocked
                {
                    edges[at] = Edge::clipped(horizontal, fixed, start, end.min(along_start));
                    edges[at + 1] = Edge::clipped(horizontal, fixed, start.max(along_end), end);
                }
                else
                {
                    edges[at] = Edge::clipped(horizontal, fixed, start, end);
                }
            }
        }
        edges
    }

    pub(crate) fn visit(self, clip: Rect, pixel: impl FnMut(usize, usize))
    {
        self.visit_difference(None, clip, pixel);
    }

    #[inline(never)]
    pub(super) fn visit_difference(self, other: Option<Self>, clip: Rect,
                                   mut pixel: impl FnMut(usize, usize))
    {
        if clip.is_empty() || !self.intersects_perimeter(clip)
        {
            return;
        }
        let edges = self.edges();
        let other_edges = other.map_or([None; 16], Self::edges);
        for (index, edge) in edges.iter().enumerate()
        {
            let Some(edge) = edge.and_then(|edge| edge.intersection(clip))
            else
            {
                continue;
            };
            // Subtract each previously visited edge and the other outline once
            // per span. This also removes shared corners and crossing pixels.
            // At most 15 prior + 16 other intervals can split the initial span:
            // 33 slots suffice without allocation or a fallible paint path.
            let mut spans = [(0, 0); 33];
            spans[0] = (edge.start, edge.end);
            let mut count = 1;
            for prior in edges[..index].iter().chain(other_edges.iter()).flatten()
            {
                let Some((from, to)) = edge.covered_by(*prior)
                else
                {
                    continue;
                };
                let mut i = 0;
                while i < count
                {
                    let (start, end) = spans[i];
                    let (a, b) = (start.max(from), end.min(to));
                    if a >= b
                    {
                        i += 1;
                        continue;
                    }
                    if a == start && b == end
                    {
                        count -= 1;
                        spans[i] = spans[count];
                        continue;
                    }
                    if a > start
                    {
                        spans[i].1 = a;
                        if b < end
                        {
                            spans[count] = (b, end);
                            count += 1;
                        }
                    }
                    else
                    {
                        spans[i].0 = b;
                    }
                    i += 1;
                }
                if count == 0
                {
                    break;
                }
            }
            for &(start, end) in &spans[..count]
            {
                let start = start + i32::from((start + edge.fixed) & 1 == 0);
                for along in (start..end).step_by(2)
                {
                    let (x, y) =
                        if edge.horizontal { (along, edge.fixed) } else { (edge.fixed, along) };
                    pixel(x as usize, y as usize);
                }
            }
        }
    }
}

/// Half-open, axis-aligned pixel span. Coordinates are clipped to the supported
/// surface range once, before interval arithmetic or pixel access.
#[derive(Clone, Copy)]
struct Edge
{
    horizontal: bool,
    fixed:      i32,
    start:      i32,
    end:        i32
}
impl Edge
{
    fn clipped(horizontal: bool, fixed: i64, start: i64, end: i64) -> Option<Self>
    {
        let fixed_min = if horizontal { 21 } else { 0 };
        let along_min = if horizontal { 0 } else { 21 };
        let start = start.max(along_min);
        let end = end.min(i64::from(i16::MAX));
        (fixed >= fixed_min && fixed < i64::from(i16::MAX) && start < end)
            .then_some(Self { horizontal,fixed: fixed as i32,start: start as i32,end: end as i32 })
    }

    fn intersection(self, clip: Rect) -> Option<Self>
    {
        let (from, to, low, high) = if self.horizontal
        {
            (clip.left, clip.right, clip.top, clip.bottom)
        }
        else
        {
            (clip.top, clip.bottom, clip.left, clip.right)
        };
        let start = self.start.max(i32::from(from));
        let end = self.end.min(i32::from(to));
        (self.fixed >= i32::from(low) && self.fixed < i32::from(high) && start < end)
            .then_some(Self { start,end,..self })
    }

    fn covered_by(self, other: Self) -> Option<(i32, i32)>
    {
        if self.horizontal == other.horizontal
        {
            (self.fixed == other.fixed).then_some((other.start, other.end))
        }
        else
        {
            (self.fixed >= other.start && self.fixed < other.end).then_some((other.fixed,
                                                                             other.fixed + 1))
        }
    }
}

/// Original indexed-device XOR profile; other RGB colors use direct inversion.
pub(crate) fn indexed_tracking_xor(rgb: u32) -> u32
{
    const TABLE: &[u8] = include_bytes!("../../../assets/windows/system7-indexed-xor.bin");
    let entries = TABLE.as_chunks::<8>().0;
    entries.binary_search_by_key(&rgb, |e| u32::from_be_bytes([e[0], e[1], e[2], e[3]]))
           .map_or(rgb ^ 0xffffff, |i| {
               let e = entries[i];
               u32::from_be_bytes([e[4], e[5], e[6], e[7]])
           })
}

#[derive(Default)]
pub(crate) struct Overlays<'a>
{
    pub previous_cursor:   Rect,
    pub cursor:            Option<CursorSprite<'a>>,
    pub cursor_changed:    bool,
    pub previous_feedback: Option<&'a Feedback>,
    pub feedback:          Option<&'a Feedback>
}

pub(crate) enum PresentEvent
{
    CopyStarted,
    Copied(Rect),
    OutlineStarted,
    OutlineChanged(usize)
}

/// Copy scene damage with the old outline, remove the old cursor, XOR only the
/// changed outline pixels, then publish the cursor above the new outline.
/// The scene never contains either overlay, including while content is painted.
pub(crate) fn present_frame(scene: &[u32], front: &mut [u32], width: usize, height: usize,
                            damage: impl IntoIterator<Item = Rect>, overlays: Overlays<'_>,
                            mut observe: impl FnMut(PresentEvent))
                            -> Option<usize>
{
    valid_surface(scene, front, width, height)?;
    let empty = Feedback::default();
    let previous = overlays.previous_feedback.unwrap_or(&empty);
    let current = overlays.feedback.unwrap_or(&empty);
    // A stationary overlay needs no separate erase/redraw. Composite only the
    // exact scene rectangles, including their cursor/outline intersections.
    if !overlays.cursor_changed && previous == current
    {
        for rect in damage
        {
            observe(PresentEvent::CopyStarted);
            if let Some(rect) =
                copy_rect(scene, front, width, height, rect, current, overlays.cursor)
            {
                observe(PresentEvent::Copied(rect));
            }
        }
        return Some(0);
    }
    let mut old_cursor_restored = overlays.previous_cursor.is_empty();
    for rect in damage
    {
        observe(PresentEvent::CopyStarted);
        if let Some(rect) = copy_rect(scene, front, width, height, rect, previous, None)
        {
            let old = overlays.previous_cursor;
            old_cursor_restored |= rect.left <= old.left
                                   && rect.top <= old.top
                                   && rect.right >= old.right
                                   && rect.bottom >= old.bottom;
            observe(PresentEvent::Copied(rect));
        }
    }
    if !old_cursor_restored
    {
        observe(PresentEvent::CopyStarted);
        if let Some(rect) =
            copy_rect(scene, front, width, height, overlays.previous_cursor, previous, None)
        {
            observe(PresentEvent::Copied(rect));
        }
    }
    let mut changed = 0;
    if previous != current
    {
        observe(PresentEvent::OutlineStarted);
        let clip = Rect::new(0, 0, width as i16, height as i16);
        changed = current.transition(previous, scene, front, width, clip);
        observe(PresentEvent::OutlineChanged(changed));
    }
    if let Some(cursor) = overlays.cursor
    {
        observe(PresentEvent::CopyStarted);
        if let Some(rect) =
            copy_rect(scene, front, width, height, cursor.bounds(), current, Some(cursor))
        {
            observe(PresentEvent::Copied(rect));
        }
    }
    Some(changed)
}

fn valid_surface(scene: &[u32], front: &[u32], width: usize, height: usize) -> Option<()>
{
    let length = width.checked_mul(height)?;
    (width <= i16::MAX as usize
     && height <= i16::MAX as usize
     && scene.len() >= length
     && front.len() >= length)
                              .then_some(())
}

#[derive(Clone, Copy)]
pub(crate) struct CursorSprite<'a>
{
    image:  MonoCursor,
    colors: &'a [u32],
    left:   i32,
    top:    i32
}
impl<'a> CursorSprite<'a>
{
    pub(super) fn new(image: MonoCursor, colors: &'a [u32], mouse: Point) -> Self
    {
        Self { image,
               colors,
               left: i32::from(mouse.h) - i32::from(image.hotspot.h),
               top: i32::from(mouse.v) - i32::from(image.hotspot.v) }
    }

    pub(crate) fn bounds(self) -> Rect
    {
        let coordinate = |value: i32| value.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        Rect::new(coordinate(self.left),
                  coordinate(self.top),
                  coordinate(self.left + 16),
                  coordinate(self.top + 16))
    }

    fn composite_row(self, pixels: &mut [u32], left: i32, y: i32)
    {
        let row = y - self.top;
        if !(0..16).contains(&row)
        {
            return;
        }
        let start = self.left.max(left);
        let end = (self.left + 16).min(left + pixels.len() as i32);
        for x in start..end
        {
            let column = (x - self.left) as usize;
            let row = row as usize;
            let pixel = &mut pixels[(x - left) as usize];
            let bit = 0x8000 >> column;
            let image = self.image.image[row] & bit != 0;
            let mask = self.image.mask[row] & bit != 0;
            if self.colors.len() == 256
            {
                let color = self.colors[row * 16 + column] & 0xffffff;
                if mask
                {
                    *pixel = color;
                }
                else
                {
                    *pixel ^= !color & 0xffffff;
                }
            }
            else if mask
            {
                *pixel = if image { 0 } else { 0xffffff };
            }
            else if image
            {
                *pixel ^= 0xffffff;
            }
        }
    }
}

/// Copy one clipped damage rectangle and composite only its cursor pixels.
/// Refuse invalid surface extents before writing any frontbuffer pixels.
#[cfg(not(target_arch = "riscv64"))]
pub(crate) fn present_rect(scene: &[u32], front: &mut [u32], width: usize, height: usize,
                           rect: Rect, cursor: Option<CursorSprite<'_>>)
                           -> Option<Rect>
{
    valid_surface(scene, front, width, height)?;
    copy_rect(scene, front, width, height, rect, &Feedback::default(), cursor)
}

// One shared copy/overlay path keeps the cold instruction footprint bounded;
// cloning it into each erase/damage/cursor call site delays the first drag move.
#[inline(never)]
fn copy_rect(scene: &[u32], front: &mut [u32], width: usize, height: usize, rect: Rect,
             feedback: &Feedback, cursor: Option<CursorSprite<'_>>)
             -> Option<Rect>
{
    let rect = Rect::new(rect.left.clamp(0, width as i16),
                         rect.top.clamp(0, height as i16),
                         rect.right.clamp(0, width as i16),
                         rect.bottom.clamp(0, height as i16));
    if rect.is_empty()
    {
        return None;
    }
    let left = rect.left as usize;
    let right = rect.right as usize;
    for y in rect.top as usize..rect.bottom as usize
    {
        let at = y * width;
        let output = &mut front[at + left..at + right];
        output.copy_from_slice(&scene[at + left..at + right]);
    }
    feedback.paint(scene, front, width, rect);
    if let Some(cursor) = cursor
    {
        let mut overlap = Rect::ZERO;
        if super::geometry::sect_rect(rect, cursor.bounds(), &mut overlap)
        {
            for y in overlap.top as usize..overlap.bottom as usize
            {
                let at = y * width;
                cursor.composite_row(&mut front
                                         [at + overlap.left as usize..at + overlap.right as usize],
                                     i32::from(overlap.left),
                                     y as i32);
            }
        }
    }
    Some(rect)
}
