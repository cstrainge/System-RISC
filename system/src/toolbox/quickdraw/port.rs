//! Stateful, safe QuickDraw primitives. Wire requests and pointers stay outside.
#![forbid(unsafe_code)]

use alloc::vec::Vec;

use super::{Canvas, Point, Rect,
            raster::TransferMode,
            region::{BooleanOp, Region}};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortState
{
    pub pen_location:       Point,
    pub pen_size:           Point,
    pub pen_mode:           TransferMode,
    pub pen_pattern:        [u8; 8],
    pub background_pattern: [u8; 8],
    pub foreground:         u32,
    pub background:         u32,
    pub pen_visibility:     i16,
    pub clip:               Option<Region>
}
impl Default for PortState
{
    fn default() -> Self
    {
        Self { pen_location:       Point::default(),
               pen_size:           Point { h: 1,
                                           v: 1 },
               pen_mode:           TransferMode::PAT_COPY,
               pen_pattern:        [255; 8],
               background_pattern: [0; 8],
               foreground:         0,
               background:         0xffffff,
               pen_visibility:     0,
               clip:               None }
    }
}
impl PortState
{
    pub fn pen_normal(&mut self)
    {
        self.pen_size = Point { h: 1,
                                v: 1 };
        self.pen_mode = TransferMode::PAT_COPY;
        self.pen_pattern = [255; 8];
    }

    pub fn move_to(&mut self, p: Point)
    {
        self.pen_location = p;
    }

    pub fn move_by(&mut self, delta: Point)
    {
        self.pen_location.h = self.pen_location.h.wrapping_add(delta.h);
        self.pen_location.v = self.pen_location.v.wrapping_add(delta.v);
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Shape<'a>
{
    Rect(Rect),
    Oval(Rect),
    RoundRect
    {
        bounds:      Rect,
        oval_width:  i16,
        oval_height: i16
    },
    Arc
    {
        bounds: Rect,
        start:  i16,
        sweep:  i16
    },
    Polygon(&'a [Point]),
    Region(&'a Region)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb
{
    Frame,
    Paint,
    Erase,
    Invert,
    Fill([u8; 8])
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicsError
{
    InvalidSize,
    TooLarge,
    CoordinateOverflow
}
const MAX_PIXELS: i64 = 16_777_216;

fn bounds(shape: Shape<'_>) -> Rect
{
    match shape
    {
        Shape::Rect(r)
        | Shape::Oval(r)
        | Shape::RoundRect { bounds: r,
                           .. }
        | Shape::Arc { bounds: r,
                     .. } => r,
        Shape::Region(r) => r.bounds(),
        Shape::Polygon(points) =>
        {
            if points.is_empty()
            {
                Rect::ZERO
            }
            else
            {
                Rect::new(points.iter().map(|p| p.h).min().unwrap(),
                          points.iter().map(|p| p.v).min().unwrap(),
                          points.iter().map(|p| p.h).max().unwrap(),
                          points.iter().map(|p| p.v).max().unwrap())
            }
        }
    }
}
fn intersect(a: Rect, b: Rect) -> Rect
{
    let mut r = Rect::ZERO;
    super::geometry::sect_rect(a, b, &mut r);
    r
}
fn check_area(rect: Rect) -> Result<(), GraphicsError>
{
    let w = i64::from(rect.right) - i64::from(rect.left);
    let h = i64::from(rect.bottom) - i64::from(rect.top);
    if w.max(0) * h.max(0) > MAX_PIXELS { Err(GraphicsError::TooLarge) } else { Ok(()) }
}
/// Original Color QuickDraw's fixed-point oval edge recurrence, reconstructed
/// from ROM helpers 0x40834d90/0x40834e40 and checked against original regions.
/// FixRatio truncation and the retained edge state affect thin and odd ovals.
fn oval_region(r: Rect, clip: Rect, arc: Option<(i16, i16)>) -> Result<Region, GraphicsError>
{
    if r.is_empty() || clip.is_empty()
    {
        return Ok(Region::new());
    }
    check_area(r)?;
    let width = i128::from(r.right) - i128::from(r.left);
    let height = i128::from(r.bottom) - i128::from(r.top);
    let ratio = (height << 16) / width;
    let square = ratio * ratio;
    let mut target = height * 2 - 1;
    let mut error = 0i128;
    let mut delta = square;
    let mut left = (i128::from(r.left) << 16) + (width << 15);
    let mut right = (i128::from(r.right) << 16) - (width << 15) + 32768;
    let mut spans = Vec::with_capacity(height as usize);
    let mut min_left = i16::MAX;
    let mut max_right = i16::MIN;
    for row in 0..height
    {
        while (error >> 32) < target
        {
            left -= 32768;
            right += 32768;
            error += delta;
            delta += square * 2;
        }
        while (error >> 32) > target
        {
            left += 32768;
            right -= 32768;
            delta -= square * 2;
            error -= delta;
        }
        let left = i16::try_from(left >> 16).map_err(|_| GraphicsError::CoordinateOverflow)?;
        let right = i16::try_from(right >> 16).map_err(|_| GraphicsError::CoordinateOverflow)?;
        spans.push((left, right));
        if left < right
        {
            min_left = min_left.min(left);
            max_right = max_right.max(right);
        }
        target -= 4 * (row * 2 + 2 - height);
    }
    let bounds = intersect(Rect::new(min_left, r.top, max_right, r.bottom), clip);
    check_area(bounds)?;
    Ok(Region::from_predicate(bounds, |point| {
        let (left, right) = spans[(i32::from(point.v) - i32::from(r.top)) as usize];
        point.h >= left
        && point.h < right
        && arc.is_none_or(|(start, sweep)| wedge(r, start, sweep, point))
    }))
}
fn rounded(r: Rect, ow: i16, oh: i16, p: Point) -> bool
{
    if !r.contains(p)
    {
        return false;
    }
    let ow = i32::from(ow).min(i32::from(r.right) - i32::from(r.left));
    let oh = i32::from(oh).min(i32::from(r.bottom) - i32::from(r.top));
    if ow <= 0 || oh <= 0
    {
        return true;
    }
    let x = 2 * i32::from(p.h) + 1;
    let y = 2 * i32::from(p.v) + 1;
    let dx = if x < 2 * i32::from(r.left) + ow
    {
        x - (2 * i32::from(r.left) + ow)
    }
    else if x > 2 * i32::from(r.right) - ow
    {
        x - (2 * i32::from(r.right) - ow)
    }
    else
    {
        0
    };
    let dy = if y < 2 * i32::from(r.top) + oh
    {
        y - (2 * i32::from(r.top) + oh)
    }
    else if y > 2 * i32::from(r.bottom) - oh
    {
        y - (2 * i32::from(r.bottom) - oh)
    }
    else
    {
        0
    };
    let (dx, dy, ow, oh) = (i128::from(dx), i128::from(dy), i128::from(ow), i128::from(oh));
    dx * dx * oh * oh + dy * dy * ow * ow <= ow * ow * oh * oh
}
fn polygon(points: &[Point], p: Point) -> bool
{
    if points.len() < 3
    {
        return false;
    }
    let mut inside = false;
    for (first, last) in points.iter().zip(points.iter().cycle().skip(1)).take(points.len())
    {
        let (mut a, mut b) = (*first, *last);
        if a.v > b.v
        {
            core::mem::swap(&mut a, &mut b);
        }
        if p.v < a.v || p.v >= b.v
        {
            continue;
        }
        let dx = i64::from(b.h) - i64::from(a.h);
        let dy = i64::from(b.v) - i64::from(a.v);
        let slope = (dx << 16) / dy;
        let crossing =
            ((i64::from(a.h) << 16) + (i64::from(p.v) - i64::from(a.v)) * slope + 32768) >> 16;
        let crossing = crossing + i64::from(dx > 0);
        if i64::from(p.h) < crossing
        {
            inside = !inside;
        }
    }
    inside
}
const SIN: [i32; 91] =
    [0, 1144, 2287, 3430, 4572, 5712, 6850, 7987, 9121, 10252, 11380, 12505, 13626, 14742, 15855,
     16962, 18064, 19161, 20252, 21336, 22415, 23486, 24550, 25607, 26656, 27697, 28729, 29753,
     30767, 31772, 32768, 33754, 34729, 35693, 36647, 37590, 38521, 39441, 40348, 41243, 42126,
     42995, 43852, 44695, 45525, 46341, 47143, 47930, 48703, 49461, 50203, 50931, 51643, 52339,
     53020, 53684, 54332, 54963, 55578, 56175, 56756, 57319, 57865, 58393, 58903, 59396, 59870,
     60326, 60764, 61183, 61584, 61966, 62328, 62672, 62997, 63303, 63589, 63856, 64104, 64332,
     64540, 64729, 64898, 65048, 65177, 65287, 65376, 65446, 65496, 65526, 65536];
fn sin(angle: i32) -> i64
{
    let a = angle.rem_euclid(360);
    match a
    {
        0..=90 => i64::from(SIN[a as usize]),
        91..=180 => i64::from(SIN[(180 - a) as usize]),
        181..=270 => -i64::from(SIN[(a - 180) as usize]),
        _ => -i64::from(SIN[(360 - a) as usize])
    }
}
fn wedge(r: Rect, start: i16, sweep: i16, p: Point) -> bool
{
    let mut start = i32::from(start);
    let mut sweep = i32::from(sweep);
    if sweep.abs() >= 360
    {
        return true;
    }
    if sweep == 0
    {
        return false;
    }
    if sweep < 0
    {
        start += sweep;
        sweep = -sweep;
    }
    // The original arc scan converter tests the upper-right pixel corner;
    // sampling pixel centers changes radial edges by one pixel.
    let center_h = (i64::from(r.left) + i64::from(r.right)).div_euclid(2);
    let center_v = (i64::from(r.top) + i64::from(r.bottom)).div_euclid(2);
    let x = 2 * (i64::from(p.h) + 1 - center_h) * (i64::from(r.bottom) - i64::from(r.top));
    let y = 2 * (i64::from(p.v) - center_v) * (i64::from(r.right) - i64::from(r.left));
    if x == 0 && y == 0
    {
        return sweep > 180;
    }
    let (ax, ay) = (sin(start), -sin(start + 90));
    let (bx, by) = (sin(start + sweep), -sin(start + sweep + 90));
    let first = ax * y - ay * x >= 0;
    let second = x * by - y * bx >= 0;
    if sweep <= 180 { first && second } else { first || second }
}
fn contains(shape: Shape<'_>, p: Point) -> bool
{
    match shape
    {
        Shape::Rect(r) => r.contains(p),
        Shape::Oval(_) | Shape::Arc { .. } =>
        {
            unreachable!("curves use fixed-point scan conversion")
        }
        Shape::RoundRect { bounds,
                           oval_width,
                           oval_height } => rounded(bounds, oval_width, oval_height, p),
        Shape::Polygon(points) => polygon(points, p),
        Shape::Region(r) => r.contains(p)
    }
}
fn validate(shape: Shape<'_>) -> Result<(), GraphicsError>
{
    if let Shape::RoundRect { oval_width,
                              oval_height,
                              .. } = shape
       && (oval_width < 0 || oval_height < 0)
    {
        return Err(GraphicsError::InvalidSize);
    }
    Ok(())
}
fn region_clipped(shape: Shape<'_>, clip: Rect) -> Result<Region, GraphicsError>
{
    validate(shape)?;
    match shape
    {
        Shape::Oval(rect) => return oval_region(rect, clip, None),
        Shape::Arc { bounds,
                     start,
                     sweep } => return oval_region(bounds, clip, Some((start, sweep))),
        _ =>
        {}
    }
    let rect = intersect(bounds(shape), clip);
    check_area(rect)?;
    Ok(Region::from_predicate(rect, |p| contains(shape, p)))
}
pub fn shape_region(shape: Shape<'_>) -> Result<Region, GraphicsError>
{
    validate(shape)?;
    check_area(bounds(shape))?;
    Ok(match shape
    {
        Shape::Oval(rect) =>
        {
            oval_region(rect, Rect::new(i16::MIN, i16::MIN, i16::MAX, i16::MAX), None)?
        }
        Shape::Arc { bounds,
                     start,
                     sweep } =>
        {
            oval_region(bounds,
                        Rect::new(i16::MIN, i16::MIN, i16::MAX, i16::MAX),
                        Some((start, sweep)))?
        }
        Shape::Rect(r) => Region::from_rect(r),
        Shape::Region(r) => r.clone(),
        _ => Region::from_predicate(bounds(shape), |p| contains(shape, p))
    })
}
fn local_clip(canvas: &Canvas) -> Rect
{
    let o = canvas.origin();
    let c = canvas.clip();
    Rect::new((i32::from(c.left) - i32::from(o.h)).clamp(i32::from(i16::MIN), i32::from(i16::MAX))
              as i16,
              (i32::from(c.top) - i32::from(o.v)).clamp(i32::from(i16::MIN), i32::from(i16::MAX))
              as i16,
              (i32::from(c.right) - i32::from(o.h)).clamp(i32::from(i16::MIN), i32::from(i16::MAX))
              as i16,
              (i32::from(c.bottom) - i32::from(o.v)).clamp(i32::from(i16::MIN), i32::from(i16::MAX))
              as i16)
}
fn paint(canvas: &mut Canvas, state: &PortState, region: Region, verb: Verb)
{
    let region = if let Some(clip) = &state.clip
    {
        region.combine(clip, BooleanOp::Intersection)
    }
    else
    {
        region
    };
    let o = canvas.origin();
    for band in region.bands()
    {
        for y in band.top..band.bottom
        {
            for span in &band.spans
            {
                for x in span.left..span.right
                {
                    let offset = (i32::from(y) + i32::from(o.v)) as usize * canvas.width
                                 + (i32::from(x) + i32::from(o.h)) as usize;
                    let destination = canvas.pixels[offset];
                    let pattern = match verb
                    {
                        Verb::Fill(pattern) => pattern,
                        Verb::Erase => state.background_pattern,
                        _ => state.pen_pattern
                    };
                    let bit =
                        pattern[(i32::from(y) & 7) as usize] & (0x80 >> (i32::from(x) & 7)) != 0;
                    canvas.pixels[offset] = match verb
                    {
                        Verb::Invert => (!destination) & 0xffffff,
                        Verb::Erase =>
                        {
                            TransferMode::PAT_COPY.pattern(bit, destination, state.foreground,
                                                           state.background)
                        }
                        Verb::Fill(_) =>
                        {
                            TransferMode::PAT_COPY.pattern(bit, destination, state.foreground,
                                                           state.background)
                        }
                        _ =>
                        {
                            state.pen_mode
                                 .pattern(bit, destination, state.foreground, state.background)
                        }
                    };
                }
            }
        }
    }
}
fn inset(r: Rect, h: i16, v: i16) -> Rect
{
    let left = i32::from(r.left) + i32::from(h);
    let top = i32::from(r.top) + i32::from(v);
    let right = i32::from(r.right) - i32::from(h);
    let bottom = i32::from(r.bottom) - i32::from(v);
    if left >= right || top >= bottom
    {
        return Rect::ZERO;
    }
    Rect::new(left as i16, top as i16, right as i16, bottom as i16)
}
fn line_region(start: Point, end: Point, size: Point, clip: Rect) -> Result<Region, GraphicsError>
{
    if size.h < 0 || size.v < 0
    {
        return Err(GraphicsError::InvalidSize);
    }
    if size.h == 0 || size.v == 0
    {
        return Ok(Region::new());
    }
    let mut result = Region::new();
    // Original StdLine canonicalizes toward increasing vertical coordinates.
    // The two major-axis paths use different 16.16 stepping and tie rules.
    // These paths are checked against 88 captured original line point sets.
    let (mut first, mut last) = (start, end);
    if first.v > last.v || (first.v == last.v && first.h > last.h)
    {
        core::mem::swap(&mut first, &mut last);
    }
    let dx = i64::from(last.h) - i64::from(first.h);
    let dy = i64::from(last.v) - i64::from(first.v);
    let horizontal = dx.abs() >= dy;
    let major = dx.abs().max(dy);
    let minor = dx.abs().min(dy);
    let direction = if dx >= 0 { 1 } else { -1 };
    let inverse = if minor != 0 { (major << 16) / minor } else { 0 };
    let slope = if major != 0 { (minor << 16) / major } else { 0 };
    let mut secondary = 0;
    for step in 0..=major
    {
        let next = if horizontal && major != minor
        {
            while secondary < minor
                  && step
                     >= (inverse * (2 * (secondary + 1) - 1)
                         + if direction > 0 { 65536 } else { 65535 })
                        / 131072
            {
                secondary += 1;
            }
            secondary
        }
        else if major == minor
        {
            step
        }
        else
        {
            (((2 * step + 1) * slope + 65536) / 131072).min(minor)
        };
        let (x, y) = if horizontal
        {
            (i64::from(first.h) + step * direction, i64::from(first.v) + next)
        }
        else
        {
            (i64::from(first.h) + next * direction, i64::from(first.v) + step)
        };
        let right = (x + i64::from(size.h)).min(i64::from(i16::MAX));
        let bottom = (y + i64::from(size.v)).min(i64::from(i16::MAX));
        result = result.combine(&Region::from_rect(intersect(Rect::new(x as i16, y as i16,
                                                                       right as i16,
                                                                       bottom as i16),
                                                             clip)),
                                BooleanOp::Union);
    }
    Ok(result)
}
pub fn line_to(canvas: &mut Canvas, state: &mut PortState, end: Point)
               -> Result<(), GraphicsError>
{
    let start = state.pen_location;
    state.pen_location = end;
    if state.pen_visibility < 0
    {
        return Ok(());
    }
    let region = line_region(start, end, state.pen_size, local_clip(canvas))?;
    paint(canvas, state, region, Verb::Paint);
    Ok(())
}
pub fn draw(canvas: &mut Canvas, state: &PortState, shape: Shape<'_>, verb: Verb)
            -> Result<(), GraphicsError>
{
    validate(shape)?;
    if state.pen_size.h < 0 || state.pen_size.v < 0
    {
        return Err(GraphicsError::InvalidSize);
    }
    if state.pen_visibility < 0 && matches!(verb, Verb::Frame | Verb::Paint)
    {
        return Ok(());
    }
    let clip = local_clip(canvas);
    let mut region = region_clipped(shape, clip)?;
    if verb == Verb::Frame
    {
        if state.pen_size.h == 0 || state.pen_size.v == 0
        {
            return Ok(());
        }
        if let Shape::Polygon(points) = shape
        {
            region = Region::new();
            for (a, b) in points.iter().zip(points.iter().cycle().skip(1)).take(points.len())
            {
                region =
                    region.combine(&line_region(*a, *b, state.pen_size, clip)?, BooleanOp::Union);
            }
        }
        else
        {
            let inner=match shape{
    Shape::Rect(r)=>region_clipped(Shape::Rect(inset(r,state.pen_size.h,state.pen_size.v)),clip)?,
    Shape::Oval(r)|Shape::Arc{bounds:r,..}=>region_clipped(Shape::Oval(inset(r,state.pen_size.h,state.pen_size.v)),clip)?,
    Shape::RoundRect{bounds,oval_width,oval_height}=>region_clipped(Shape::RoundRect{bounds:inset(bounds,state.pen_size.h,state.pen_size.v),oval_width:oval_width.saturating_sub(state.pen_size.h.saturating_mul(2)).max(0),oval_height:oval_height.saturating_sub(state.pen_size.v.saturating_mul(2)).max(0)},clip)?,
    Shape::Region(r)=>r.inset(state.pen_size.h,state.pen_size.v)
                    .map_err(|_|GraphicsError::CoordinateOverflow)?
                    .combine(&Region::from_rect(clip),BooleanOp::Intersection),
Shape::Polygon(_)=>unreachable!(),
   };
            region = region.combine(&inner, BooleanOp::Difference);
        }
    }
    paint(canvas, state, region, verb);
    Ok(())
}

#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn original_thin_ovals_keep_fixed_ratio_rounding_and_asymmetric_edges()
    {
        // Exact OpenRgn/FrameOval/CloseRgn output from original System 7.6.1.
        let cases = [(3,1,"000a0000ffff00010004"),
                     (2,8,"000a0001000000070002"),
                     (16,3,"0034000000000003001000000002000e7fff000100000002000e00107fff000200000001000f00107fff00030001000f7fff7fff")];
        for (width, height, hex) in cases
        {
            let bytes: Vec<u8> =
                hex.as_bytes()
                   .as_chunks::<2>()
                   .0
                   .iter()
                   .map(|pair| {
                       let digit = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
                       digit(pair[0]) * 16 + digit(pair[1])
                   })
                   .collect();
            let region = shape_region(Shape::Oval(Rect::new(0, 0, width, height))).unwrap();
            assert_eq!(region.to_be_bytes().unwrap(), bytes);
        }
        let mut pixels = [0xffffff; 8];
        let mut canvas = Canvas::new(&mut pixels, 8, 1);
        // The one-row curve reaches one pixel beyond each supplied horizontal edge.
        draw(&mut canvas,&PortState::default(),Shape::Oval(Rect::new(2,0,5,1)),Verb::Paint).unwrap();
        assert_eq!(pixels, [0xffffff, 0, 0, 0, 0, 0, 0xffffff, 0xffffff]);
    }

    #[test]
    fn concave_polygon_and_clipped_stateful_pens()
    {
        let points = [Point { h: 1,
                              v: 1 },
                      Point { h: 7,
                              v: 1 },
                      Point { h: 4,
                              v: 4 },
                      Point { h: 7,
                              v: 7 },
                      Point { h: 1,
                              v: 7 }];
        let r = shape_region(Shape::Polygon(&points)).unwrap();
        assert!(r.contains(Point { h: 2,
                                   v: 3 }));
        assert!(!r.contains(Point { h: 6,
                                    v: 3 }));
        let mut pixels = [0xffffff; 100];
        let mut canvas = Canvas::new(&mut pixels, 10, 10);
        let mut state = PortState { clip: Some(Region::from_rect(Rect::new(2, 2, 8, 8))),
                                    pen_size: Point { h: 3,
                                                      v: 2 },
                                    ..PortState::default() };
        line_to(&mut canvas,
                &mut state,
                Point { h: 9,
                        v: 9 }).unwrap();
        assert_eq!(state.pen_location,
                   Point { h: 9,
                           v: 9 });
        for y in 0..10
        {
            for x in 0..10
            {
                if x < 2 || y < 2 || x >= 8 || y >= 8
                {
                    assert_eq!(canvas.pixels[y * 10 + x], 0xffffff);
                }
            }
        }
    }
    #[test]
    fn frame_stays_inside_and_xor_double_draw_restores()
    {
        let mut pixels = [0xabcdef; 400];
        let mut canvas = Canvas::new(&mut pixels, 20, 20);
        let state = PortState { pen_size: Point { h: 3,
                                                  v: 2 },
                                pen_mode: TransferMode::new(10).unwrap(),
                                ..PortState::default() };
        for _ in 0..2
        {
            draw(&mut canvas, &state, Shape::Oval(Rect::new(2, 2, 18, 18)), Verb::Frame).unwrap();
        }
        assert!(canvas.pixels.iter().all(|p| *p == 0xabcdef));
        draw(&mut canvas,
             &PortState::default(),
             Shape::Arc { bounds: Rect::new(2, 2, 18, 18),
                          start:  0,
                          sweep:  90 },
             Verb::Paint).unwrap();
        assert_eq!(canvas.pixels[5 * 20 + 12], 0);
        assert_eq!(canvas.pixels[12 * 20 + 5], 0xabcdef);
    }
}
