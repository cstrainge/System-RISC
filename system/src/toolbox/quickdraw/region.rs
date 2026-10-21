//! Exact integer regions represented as normalized horizontal bands.
#![forbid(unsafe_code)]
use alloc::{vec, vec::Vec};

use super::{Point, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span
{
    pub left:  i16,
    pub right: i16
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Band
{
    pub top:    i16,
    pub bottom: i16,
    pub spans:  Vec<Span>
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Region
{
    bands: Vec<Band>
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BooleanOp
{
    Union,
    Intersection,
    Difference,
    Xor
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionError
{
    Truncated,
    InvalidSize,
    InvalidBounds,
    InvalidData,
    CoordinateOverflow,
    TooLarge
}

impl Region
{
    pub fn new() -> Self
    {
        Self::default()
    }

    pub fn from_rect(rect: Rect) -> Self
    {
        if rect.is_empty()
        {
            return Self::new();
        }
        Self { bands: vec![Band { top:    rect.top,
                                  bottom: rect.bottom,
                                  spans:  vec![Span { left:  rect.left,
                                                      right: rect.right }] }] }
    }

    pub fn bands(&self) -> &[Band]
    {
        &self.bands
    }

    /// Sweep rectangle edges once. No per-pixel work and no repeated growing
    /// region unions when a selection changes many disjoint items together.
    pub fn from_rectangles(rectangles: impl Iterator<Item = Rect>) -> Self
    {
        let mut edges = Vec::new();
        for rect in rectangles.filter(|rect| !rect.is_empty())
        {
            edges.push((rect.top, rect.left, rect.right, true));
            edges.push((rect.bottom, rect.left, rect.right, false));
        }
        edges.sort_unstable();
        let mut active: Vec<Span> = Vec::new();
        let mut result = Self::new();
        let mut index = 0;
        while index < edges.len()
        {
            let y = edges[index].0;
            while index < edges.len() && edges[index].0 == y
            {
                let (_, left, right, begin) = edges[index];
                if begin
                {
                    active.push(Span { left,
                                       right });
                }
                else if let Some(slot) =
                    active.iter().position(|span| span.left == left && span.right == right)
                {
                    active.swap_remove(slot);
                }
                index += 1;
            }
            let Some(next) = edges.get(index)
            else
            {
                break;
            };
            active.sort_unstable_by_key(|span| (span.left, span.right));
            let mut spans: Vec<Span> = Vec::new();
            for &span in &active
            {
                if let Some(last) = spans.last_mut()
                   && span.left <= last.right
                {
                    last.right = last.right.max(span.right);
                }
                else
                {
                    spans.push(span);
                }
            }
            result.push_band(y, next.0, spans);
        }
        result
    }

    pub fn is_empty(&self) -> bool
    {
        self.bands.is_empty()
    }

    pub fn bounds(&self) -> Rect
    {
        let Some(first) = self.bands.first()
        else
        {
            return Rect::ZERO;
        };
        Rect::new(self.bands.iter().flat_map(|b| &b.spans).map(|s| s.left).min().unwrap(),
                  first.top,
                  self.bands.iter().flat_map(|b| &b.spans).map(|s| s.right).max().unwrap(),
                  self.bands.last().unwrap().bottom)
    }

    pub fn contains(&self, p: Point) -> bool
    {
        self.bands.iter().find(|b| p.v >= b.top && p.v < b.bottom).is_some_and(|b| {
                                                                      b.spans.iter().any(|s| {
                                                                                        p.h
                                                                                        >= s.left
                                                                                        && p.h
                                                                                           < s.right
                                                                                    })
                                                                  })
    }

    pub fn from_predicate(bounds: Rect, mut predicate: impl FnMut(Point) -> bool) -> Self
    {
        let mut result = Self::new();
        for y in bounds.top..bounds.bottom
        {
            let mut spans = Vec::new();
            let mut start = None;
            for x in bounds.left..bounds.right
            {
                if predicate(Point { h: x,
                                     v: y })
                {
                    start.get_or_insert(x);
                }
                else if let Some(left) = start.take()
                {
                    spans.push(Span { left,
                                      right: x });
                }
            }
            if let Some(left) = start
            {
                spans.push(Span { left,
                                  right: bounds.right });
            }
            result.push_band(y, y + 1, spans);
        }
        result
    }

    fn push_band(&mut self, top: i16, bottom: i16, spans: Vec<Span>)
    {
        if top >= bottom || spans.is_empty()
        {
            return;
        }
        if let Some(last) = self.bands.last_mut()
           && last.bottom == top
           && last.spans == spans
        {
            last.bottom = bottom;
            return;
        }
        self.bands.push(Band { top,
                               bottom,
                               spans });
    }

    fn spans_at(&self, y: i16) -> &[Span]
    {
        self.bands.iter().find(|b| y >= b.top && y < b.bottom).map_or(&[], |b| &b.spans)
    }

    pub fn combine(&self, other: &Self, operation: BooleanOp) -> Self
    {
        let mut ys: Vec<i16> =
            self.bands.iter().chain(&other.bands).flat_map(|b| [b.top, b.bottom]).collect();
        ys.sort_unstable();
        ys.dedup();
        let mut result = Self::new();
        for pair in ys.windows(2)
        {
            let a = self.spans_at(pair[0]);
            let b = other.spans_at(pair[0]);
            let mut xs: Vec<i16> = a.iter().chain(b).flat_map(|s| [s.left, s.right]).collect();
            xs.sort_unstable();
            xs.dedup();
            let mut spans: Vec<Span> = Vec::new();
            for x in xs.windows(2)
            {
                let aa = a.iter().any(|s| x[0] >= s.left && x[0] < s.right);
                let bb = b.iter().any(|s| x[0] >= s.left && x[0] < s.right);
                let inside = match operation
                {
                    BooleanOp::Union => aa || bb,
                    BooleanOp::Intersection => aa && bb,
                    BooleanOp::Difference => aa && !bb,
                    BooleanOp::Xor => aa != bb
                };
                if inside
                {
                    if let Some(last) = spans.last_mut()
                       && last.right == x[0]
                    {
                        last.right = x[1];
                    }
                    else
                    {
                        spans.push(Span { left:  x[0],
                                          right: x[1] });
                    }
                }
            }
            result.push_band(pair[0], pair[1], spans);
        }
        result
    }

    /// Allocation-free overlap test over normalized, ordered bands/spans.
    pub fn overlaps(&self, other: &Self) -> bool
    {
        let (mut a, mut b) = (0, 0);
        while let (Some(left), Some(right)) = (self.bands.get(a), other.bands.get(b))
        {
            if left.bottom <= right.top
            {
                a += 1;
                continue;
            }
            if right.bottom <= left.top
            {
                b += 1;
                continue;
            }
            let (mut x, mut y) = (0, 0);
            while let (Some(l), Some(r)) = (left.spans.get(x), right.spans.get(y))
            {
                if l.right <= r.left
                {
                    x += 1;
                }
                else if r.right <= l.left
                {
                    y += 1;
                }
                else
                {
                    return true;
                }
            }
            if left.bottom <= right.bottom
            {
                a += 1;
            }
            else
            {
                b += 1;
            }
        }
        false
    }

    pub fn offset(&self, dh: i16, dv: i16) -> Result<Self, RegionError>
    {
        let mut result = self.clone();
        for band in &mut result.bands
        {
            band.top = band.top.checked_add(dv).ok_or(RegionError::CoordinateOverflow)?;
            band.bottom = band.bottom.checked_add(dv).ok_or(RegionError::CoordinateOverflow)?;
            for span in &mut band.spans
            {
                span.left = span.left.checked_add(dh).ok_or(RegionError::CoordinateOverflow)?;
                span.right = span.right.checked_add(dh).ok_or(RegionError::CoordinateOverflow)?;
            }
        }
        Ok(result)
    }

    pub fn map(&self, source: Rect, destination: Rect) -> Result<Self, RegionError>
    {
        let mut result = Self::new();
        for band in &self.bands
        {
            for span in &band.spans
            {
                let rect = super::geometry::map_rect(Rect::new(span.left, band.top, span.right,
                                                               band.bottom),
                                                     source,
                                                     destination).map_err(|_| {
                                                                     RegionError::CoordinateOverflow
                                                                 })?;
                result = result.combine(&Self::from_rect(rect), BooleanOp::Union);
            }
        }
        Ok(result)
    }

    /// Morphological inset/outset on the exact covered pixel set. Positive values
    /// erode each axis; negative values expand it. Off-domain coordinates fail.
    pub fn inset(&self, dh: i16, dv: i16) -> Result<Self, RegionError>
    {
        let mut horizontal = Self::new();
        for band in &self.bands
        {
            let mut spans: Vec<Span> = Vec::new();
            for span in &band.spans
            {
                let left = i32::from(span.left) + i32::from(dh);
                let right = i32::from(span.right) - i32::from(dh);
                if left >= right
                {
                    continue;
                }
                let left = i16::try_from(left).map_err(|_| RegionError::CoordinateOverflow)?;
                let right = i16::try_from(right).map_err(|_| RegionError::CoordinateOverflow)?;
                if let Some(last) = spans.last_mut()
                   && last.right >= left
                {
                    last.right = last.right.max(right);
                }
                else
                {
                    spans.push(Span { left,
                                      right });
                }
            }
            horizontal.push_band(band.top, band.bottom, spans);
        }
        if dv == 0 || horizontal.is_empty()
        {
            return Ok(horizontal);
        }
        let v = i32::from(dv);
        if v < 0
        {
            let mut result = Self::new();
            for band in &horizontal.bands
            {
                let top = i16::try_from(i32::from(band.top) + v).map_err(|_| {
                                                                    RegionError::CoordinateOverflow
                                                                })?;
                let bottom =
                    i16::try_from(i32::from(band.bottom) - v).map_err(|_| {
                                                                 RegionError::CoordinateOverflow
                                                             })?;
                for span in &band.spans
                {
                    result = result.combine(&Self::from_rect(Rect::new(span.left, top,
                                                                       span.right, bottom)),
                                            BooleanOp::Union);
                }
            }
            return Ok(result);
        }
        let bounds = horizontal.bounds();
        let top = i32::from(bounds.top) + v;
        let bottom = i32::from(bounds.bottom) - v;
        if top >= bottom
        {
            return Ok(Self::new());
        }
        let mut events: Vec<i32> = horizontal.bands
                                             .iter()
                                             .flat_map(|b| {
                                                 [i32::from(b.top) - v,
                                                  i32::from(b.top) + v,
                                                  i32::from(b.bottom) - v,
                                                  i32::from(b.bottom) + v]
                                             })
                                             .filter(|y| *y >= top && *y <= bottom)
                                             .collect();
        events.extend([top, bottom]);
        events.sort_unstable();
        events.dedup();
        let mut result = Self::new();
        for pair in events.windows(2)
        {
            let lo = pair[0] - v;
            let hi = pair[0] + v + 1;
            let mut cursor = lo;
            let mut intersection: Option<Self> = None;
            for band in &horizontal.bands
            {
                if i32::from(band.bottom) <= lo || i32::from(band.top) >= hi
                {
                    continue;
                }
                if i32::from(band.top) > cursor
                {
                    intersection = None;
                    break;
                }
                cursor = cursor.max(i32::from(band.bottom));
                let row = Self { bands: vec![Band { top:    0,
                                                    bottom: 1,
                                                    spans:  band.spans.clone() }] };
                intersection = Some(intersection.map_or_else(|| row.clone(),
                                                 |old| old.combine(&row, BooleanOp::Intersection)));
            }
            if cursor >= hi
               && let Some(row) = intersection
               && let Some(band) = row.bands.first()
            {
                result.push_band(pair[0] as i16, pair[1] as i16, band.spans.clone());
            }
        }
        Ok(result)
    }

    pub fn to_be_bytes(&self) -> Result<Vec<u8>, RegionError>
    {
        let bounds = self.bounds();
        let mut bytes = vec![0; 10];
        bytes[2..10].copy_from_slice(&bounds.to_be_bytes());
        if !self.is_empty() && !(self.bands.len() == 1 && self.bands[0].spans.len() == 1)
        {
            if bounds.right == i16::MAX || bounds.bottom == i16::MAX
            {
                return Err(RegionError::CoordinateOverflow);
            }
            let mut ys: Vec<i16> = self.bands.iter().flat_map(|b| [b.top, b.bottom]).collect();
            ys.sort_unstable();
            ys.dedup();
            let mut previous: Vec<i16> = Vec::new();
            for y in ys
            {
                let current: Vec<i16> =
                    self.spans_at(y).iter().flat_map(|s| [s.left, s.right]).collect();
                if current == previous
                {
                    continue;
                }
                let mut delta: Vec<i16> = previous.iter().chain(&current).copied().collect();
                delta.sort_unstable();
                let mut changes = Vec::new();
                let mut i = 0;
                while i < delta.len()
                {
                    let value = delta[i];
                    let mut n = 0;
                    while i < delta.len() && delta[i] == value
                    {
                        n += 1;
                        i += 1;
                    }
                    if n % 2 != 0
                    {
                        changes.push(value);
                    }
                }
                bytes.extend_from_slice(&y.to_be_bytes());
                for x in changes
                {
                    bytes.extend_from_slice(&x.to_be_bytes());
                }
                bytes.extend_from_slice(&i16::MAX.to_be_bytes());
                previous = current;
                if bytes.len() > 65532
                {
                    return Err(RegionError::TooLarge);
                }
            }
            bytes.extend_from_slice(&i16::MAX.to_be_bytes());
        }
        let size = u16::try_from(bytes.len()).map_err(|_| RegionError::TooLarge)?;
        bytes[..2].copy_from_slice(&size.to_be_bytes());
        Ok(bytes)
    }

    pub fn from_be_bytes(bytes: &[u8]) -> Result<Self, RegionError>
    {
        if bytes.len() < 10
        {
            return Err(RegionError::Truncated);
        }
        let size = usize::from(u16::from_be_bytes([bytes[0], bytes[1]]));
        if size != bytes.len() || size > 65534 || !size.is_multiple_of(2)
        {
            return Err(RegionError::InvalidSize);
        }
        let bounds = Rect::from_be_bytes(bytes[2..10].try_into().unwrap());
        if size == 10
        {
            if bounds.is_empty() && bounds != Rect::ZERO
            {
                return Err(RegionError::InvalidBounds);
            }
            return Ok(Self::from_rect(bounds));
        }
        if bounds.is_empty()
        {
            return Err(RegionError::InvalidBounds);
        }
        let mut cursor = 10;
        let mut active: Vec<i16> = Vec::new();
        let mut previous = None;
        let mut result = Self::new();
        let read = |cursor: &mut usize| -> Result<i16, RegionError> {
            if *cursor + 2 > bytes.len()
            {
                return Err(RegionError::Truncated);
            }
            let word = i16::from_be_bytes([bytes[*cursor], bytes[*cursor + 1]]);
            *cursor += 2;
            Ok(word)
        };
        loop
        {
            let y = read(&mut cursor)?;
            if y == i16::MAX
            {
                if cursor != size || !active.is_empty() || previous != Some(bounds.bottom)
                {
                    return Err(RegionError::InvalidData);
                }
                break;
            }
            if previous.is_some_and(|old| old >= y) || y < bounds.top || y > bounds.bottom
            {
                return Err(RegionError::InvalidData);
            }
            if let Some(top) = previous
            {
                result.push_band(top,
                                 y,
                                 active.as_chunks::<2>()
                                       .0
                                       .iter()
                                       .map(|s| {
                                           Span { left:  s[0],
                                                  right: s[1] }
                                       })
                                       .collect());
            }
            let mut delta = Vec::new();
            loop
            {
                let x = read(&mut cursor)?;
                if x == i16::MAX
                {
                    break;
                }
                if delta.last().is_some_and(|old| *old >= x) || x < bounds.left || x > bounds.right
                {
                    return Err(RegionError::InvalidData);
                }
                delta.push(x);
            }
            if delta.is_empty() || !delta.len().is_multiple_of(2)
            {
                return Err(RegionError::InvalidData);
            }
            for x in delta
            {
                match active.binary_search(&x)
                {
                    Ok(i) =>
                    {
                        active.remove(i);
                    }
                    Err(i) => active.insert(i, x)
                }
            }
            if !active.len().is_multiple_of(2)
            {
                return Err(RegionError::InvalidData);
            }
            previous = Some(y);
        }
        if result.bounds() != bounds
        {
            return Err(RegionError::InvalidBounds);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn boolean_shapes_match_pixel_truth_and_codec()
    {
        let a = Region::from_rect(Rect::new(-3, -2, 6, 7));
        let b = Region::from_rect(Rect::new(1, 1, 9, 5));
        for op in [BooleanOp::Union, BooleanOp::Intersection, BooleanOp::Difference, BooleanOp::Xor]
        {
            let result = a.combine(&b, op);
            assert_eq!(Region::from_be_bytes(&result.to_be_bytes().unwrap()).unwrap(), result);
            for y in -5..10
            {
                for x in -5..12
                {
                    let p = Point { h: x,
                                    v: y };
                    let aa = a.contains(p);
                    let bb = b.contains(p);
                    assert_eq!(result.contains(p), match op
                    {
                        BooleanOp::Union => aa || bb,
                        BooleanOp::Intersection => aa && bb,
                        BooleanOp::Difference => aa && !bb,
                        BooleanOp::Xor => aa != bb
                    });
                }
            }
        }
    }
    #[test]
    fn holes_disjoint_bands_and_malformed_records()
    {
        let a = Region::from_rect(Rect::new(0, 0, 10, 10));
        let hole = Region::from_rect(Rect::new(2, 2, 8, 8));
        let ring = a.combine(&hole, BooleanOp::Difference);
        let bytes = ring.to_be_bytes().unwrap();
        assert_eq!(Region::from_be_bytes(&bytes).unwrap(), ring);
        for n in 0..bytes.len()
        {
            assert!(Region::from_be_bytes(&bytes[..n]).is_err());
        }
        let mut bad = bytes.clone();
        bad[2] = 1;
        assert!(Region::from_be_bytes(&bad).is_err());
        assert!(ring.offset(i16::MAX, 0).is_err());
        assert_eq!(Region::new().to_be_bytes().unwrap(), vec![0, 10, 0, 0, 0, 0, 0, 0, 0, 0]);
    }
}

#[cfg(test)]
mod rectangle_sweep_tests
{
    use super::*;
    #[test]
    fn rectangle_sweep_matches_exact_union_with_touching_duplicate_and_nested_edges()
    {
        let rectangles: Vec<_> =
            (0..120).map(|index| {
                        let left = (index * 17 % 91) - 20;
                        let top = (index * 11 % 79) - 20;
                        Rect::new(left, top, left + 1 + (index % 25), top + 1 + (index % 19))
                    })
                    .chain([Rect::ZERO,
                            Rect::new(0, 0, 40, 40),
                            Rect::new(0, 0, 40, 40),
                            Rect::new(0, 40, 40, 60)])
                    .collect();
        let actual = Region::from_rectangles(rectangles.iter().copied());
        for y in -25..100
        {
            for x in -25..120
            {
                let point = Point { h: x,
                                    v: y };
                assert_eq!(actual.contains(point),
                           rectangles.iter().any(|rect| rect.contains(point)),
                           "{x},{y}");
            }
        }
        assert_eq!(Region::from_be_bytes(&actual.to_be_bytes().unwrap()).unwrap(), actual);
    }
}
