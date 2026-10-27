//! Additional checked raster operations. All buffer ownership remains in Rust.
#![forbid(unsafe_code)]
use alloc::{collections::VecDeque, vec, vec::Vec};

use super::{Canvas, Point, Rect,
            port::{self, PortState, Shape, Verb},
            raster::{self, Bitmap, RasterError, TransferMode},
            region::{BooleanOp, Region}};

fn local_surface(canvas: &Canvas) -> Result<Rect, RasterError>
{
    let origin = canvas.origin();
    Ok(Rect::new(origin.h.checked_neg().ok_or(RasterError::InvalidBounds)?,origin.v.checked_neg().ok_or(RasterError::InvalidBounds)?,i16::try_from(canvas.width as i32-i32::from(origin.h)).map_err(|_|RasterError::InvalidBounds)?,i16::try_from(canvas.height as i32-i32::from(origin.v)).map_err(|_|RasterError::InvalidBounds)?))
}
fn intersection(a: Rect, b: Rect) -> Rect
{
    let mut result = Rect::ZERO;
    super::geometry::sect_rect(a, b, &mut result);
    result
}
fn translated(rect: Rect, dh: i16, dv: i16) -> Result<Rect, RasterError>
{
    Ok(Rect::new(rect.left.checked_add(dh).ok_or(RasterError::InvalidBounds)?,
                 rect.top.checked_add(dv).ok_or(RasterError::InvalidBounds)?,
                 rect.right.checked_add(dh).ok_or(RasterError::InvalidBounds)?,
                 rect.bottom.checked_add(dv).ok_or(RasterError::InvalidBounds)?))
}
pub fn scroll_rect(canvas: &mut Canvas, rect: Rect, dh: i16, dv: i16)
                   -> Result<Region, RasterError>
{
    scroll_rect_with_state(canvas, &PortState::default(), rect, dh, dv)
}
pub fn scroll_rect_with_state(canvas: &mut Canvas, state: &PortState, rect: Rect, dh: i16, dv: i16)
                              -> Result<Region, RasterError>
{
    let origin = canvas.origin();
    let clip = canvas.clip();
    let clip = Rect::new((i32::from(clip.left) - i32::from(origin.h)).clamp(i32::from(i16::MIN),
                                                                            i32::from(i16::MAX))
                         as i16,
                         (i32::from(clip.top) - i32::from(origin.v)).clamp(i32::from(i16::MIN),
                                                                           i32::from(i16::MAX))
                         as i16,
                         (i32::from(clip.right) - i32::from(origin.h)).clamp(i32::from(i16::MIN),
                                                                             i32::from(i16::MAX))
                         as i16,
                         (i32::from(clip.bottom) - i32::from(origin.v)).clamp(i32::from(i16::MIN),
                                                                              i32::from(i16::MAX))
                         as i16);
    let mut visible = Region::from_rect(intersection(rect, clip));
    if let Some(region) = &state.clip
    {
        visible = visible.combine(region, BooleanOp::Intersection)?;
    }
    if i32::from(dh).abs() >= i32::from(rect.right) - i32::from(rect.left)
       || i32::from(dv).abs() >= i32::from(rect.bottom) - i32::from(rect.top)
    {
        port::draw(canvas, state, Shape::Region(&visible), Verb::Erase).map_err(RasterError::from)?;
        return Ok(visible);
    }
    let available = intersection(rect, local_surface(canvas)?);
    let shifted = translated(available, dh, dv)?;
    let moved = Region::from_rect(shifted).combine(&visible, BooleanOp::Intersection)?;
    if !available.is_empty()
    {
        raster::copy_within(canvas,
                            available,
                            shifted,
                            TransferMode::SRC_COPY,
                            Some(&visible),
                            0,
                            0xffffff)?;
    }
    let exposed = visible.combine(&moved, BooleanOp::Difference)?;
    port::draw(canvas, state, Shape::Region(&exposed), Verb::Erase).map_err(RasterError::from)?;
    Ok(exposed)
}

#[allow(clippy::too_many_arguments)]
pub fn copy_mask(canvas: &mut Canvas, source: &Bitmap<'_>, source_rect: Rect, mask: &Bitmap<'_>,
                 mask_rect: Rect, destination_rect: Rect, foreground: u32, background: u32)
                 -> Result<(), RasterError>
{
    let dimensions = |rect: Rect| {
        (i32::from(rect.right) - i32::from(rect.left), i32::from(rect.bottom) - i32::from(rect.top))
    };
    if dimensions(source_rect) != dimensions(mask_rect)
       || dimensions(source_rect) != dimensions(destination_rect)
    {
        return Err(RasterError::InvalidBounds);
    }
    copy_deep_mask(canvas,
                   source,
                   source_rect,
                   mask,
                   mask_rect,
                   destination_rect,
                   TransferMode::SRC_COPY,
                   None,
                   foreground,
                   background)
}
#[allow(clippy::too_many_arguments)]
pub fn copy_deep_mask(canvas: &mut Canvas, source: &Bitmap<'_>, source_rect: Rect,
                      mask: &Bitmap<'_>, mask_rect: Rect, destination_rect: Rect,
                      mode: TransferMode, clip: Option<&Region>, foreground: u32, background: u32)
                      -> Result<(), RasterError>
{
    source.validate()?;
    mask.validate()?;
    let valid = |b: Rect, r: Rect| {
        !r.is_empty()
        && r.left >= b.left
        && r.top >= b.top
        && r.right <= b.right
        && r.bottom <= b.bottom
    };
    if destination_rect.is_empty()
    {
        return Ok(());
    }
    if !valid(source.bounds(), source_rect) || !valid(mask.bounds(), mask_rect)
    {
        return Err(RasterError::InvalidBounds);
    }
    let origin = canvas.origin();
    let c = canvas.clip();
    let local = Rect::new((i32::from(c.left) - i32::from(origin.h)).clamp(i32::from(i16::MIN),
                                                                          i32::from(i16::MAX))
                          as i16,
                          (i32::from(c.top) - i32::from(origin.v)).clamp(i32::from(i16::MIN),
                                                                         i32::from(i16::MAX))
                          as i16,
                          (i32::from(c.right) - i32::from(origin.h)).clamp(i32::from(i16::MIN),
                                                                           i32::from(i16::MAX))
                          as i16,
                          (i32::from(c.bottom) - i32::from(origin.v)).clamp(i32::from(i16::MIN),
                                                                            i32::from(i16::MAX))
                          as i16);
    let area = intersection(destination_rect, local);
    let sample = |rect: Rect, x: i16, y: i16| {
        Point { h: (i64::from(rect.left)
                    + (i64::from(x) - i64::from(destination_rect.left))
                      * (i64::from(rect.right) - i64::from(rect.left))
                      / (i64::from(destination_rect.right) - i64::from(destination_rect.left)))
                   as i16,
                v: (i64::from(rect.top)
                    + (i64::from(y) - i64::from(destination_rect.top))
                      * (i64::from(rect.bottom) - i64::from(rect.top))
                      / (i64::from(destination_rect.bottom) - i64::from(destination_rect.top)))
                   as i16 }
    };
    for y in area.top..area.bottom
    {
        for x in area.left..area.right
        {
            if clip.is_some_and(|r| {
                       !r.contains(Point { h: x,
                                           v: y })
                   })
            {
                continue;
            }
            let offset = (i32::from(y) + i32::from(origin.v)) as usize * canvas.width
                         + (i32::from(x) + i32::from(origin.h)) as usize;
            let destination = canvas.pixels[offset];
            let source = mode.source(source.pixel(sample(source_rect, x, y)),
                                     destination,
                                     foreground,
                                     background);
            let mask = mask.pixel(sample(mask_rect, x, y));
            let mut output = 0;
            for shift in [0, 8, 16]
            {
                let weight = (mask >> shift) & 255;
                let a = (source >> shift) & 255;
                let b = (destination >> shift) & 255;
                output |= ((a * (255 - weight) + b * weight) / 255) << shift;
            }
            canvas.pixels[offset] = output;
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonoImage
{
    pub bounds:    Rect,
    pub row_bytes: usize,
    pub bytes:     Vec<u8>
}
impl MonoImage
{
    pub fn bitmap(&self) -> Bitmap<'_>
    {
        Bitmap::Mono { data:      &self.bytes,
                       row_bytes: self.row_bytes,
                       bounds:    self.bounds }
    }

    fn set(&mut self, x: usize, y: usize)
    {
        self.bytes[y * self.row_bytes + x / 8] |= 0x80 >> (x & 7);
    }

    fn get(&self, x: usize, y: usize) -> bool
    {
        self.bytes[y * self.row_bytes + x / 8] & (0x80 >> (x & 7)) != 0
    }
}
fn mono_output(source: &Bitmap<'_>) -> Result<MonoImage, RasterError>
{
    source.validate()?;
    if !matches!(source, Bitmap::Mono { .. })
    {
        return Err(RasterError::InvalidDepth);
    }
    let bounds = source.bounds();
    let width = (i32::from(bounds.right) - i32::from(bounds.left)) as usize;
    let height = (i32::from(bounds.bottom) - i32::from(bounds.top)) as usize;
    if width.checked_mul(height).ok_or(RasterError::TooLarge)? > 1_048_576
    {
        return Err(RasterError::TooLarge);
    }
    let row_bytes = width.div_ceil(8);
    Ok(MonoImage { bounds,
                   row_bytes,
                   bytes: vec![0; row_bytes * height] })
}
fn flood(source: &Bitmap<'_>, output: &mut MonoImage,
         starts: impl IntoIterator<Item = (usize, usize)>)
{
    let bounds = source.bounds();
    let width = (i32::from(bounds.right) - i32::from(bounds.left)) as usize;
    let height = (i32::from(bounds.bottom) - i32::from(bounds.top)) as usize;
    let mut queue = VecDeque::new();
    let white = |x: usize, y: usize| {
        source.pixel(Point { h: (i32::from(bounds.left) + x as i32) as i16,
                             v: (i32::from(bounds.top) + y as i32) as i16 })
        == 0xffffff
    };
    for (x, y) in starts
    {
        if x < width && y < height && !output.get(x, y) && white(x, y)
        {
            output.set(x, y);
            queue.push_back((x, y));
        }
    }
    while let Some((x, y)) = queue.pop_front()
    {
        for (xx, yy) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)]
        {
            if xx < width && yy < height && !output.get(xx, yy) && white(xx, yy)
            {
                output.set(xx, yy);
                queue.push_back((xx, yy));
            }
        }
    }
}
/// Return set bits for the four-connected white area containing seed; black
/// source bits are boundaries. Input and output may use different row padding.
pub fn seed_fill(source: &Bitmap<'_>, seed: Point) -> Result<MonoImage, RasterError>
{
    let mut output = mono_output(source)?;
    let bounds = source.bounds();
    if !bounds.contains(seed)
    {
        return Err(RasterError::InvalidBounds);
    }
    let x = (i32::from(seed.h) - i32::from(bounds.left)) as usize;
    let y = (i32::from(seed.v) - i32::from(bounds.top)) as usize;
    flood(source, &mut output, [(x, y)]);
    Ok(output)
}
/// Include black source bits and white holes disconnected from the outer edge.
pub fn calc_mask(source: &Bitmap<'_>) -> Result<MonoImage, RasterError>
{
    let mut outside = mono_output(source)?;
    let bounds = source.bounds();
    let width = (i32::from(bounds.right) - i32::from(bounds.left)) as usize;
    let height = (i32::from(bounds.bottom) - i32::from(bounds.top)) as usize;
    let seeds = (0..width).flat_map(|x| [(x, 0), (x, height - 1)])
                          .chain((0..height).flat_map(|y| [(0, y), (width - 1, y)]));
    flood(source, &mut outside, seeds);
    let mut result = mono_output(source)?;
    for y in 0..height
    {
        for x in 0..width
        {
            if !outside.get(x, y)
            {
                result.set(x, y);
            }
        }
    }
    Ok(result)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Color
{
    pub red:   u16,
    pub green: u16,
    pub blue:  u16
}
impl Color
{
    pub const fn from_rgb8(value: u32) -> Self
    {
        Self { red:   (((value >> 16) & 255) * 257) as u16,
               green: (((value >> 8) & 255) * 257) as u16,
               blue:  ((value & 255) * 257) as u16 }
    }

    pub const fn rgb8(self) -> u32
    {
        ((self.red as u32 >> 8) << 16) | ((self.green as u32 >> 8) << 8) | (self.blue as u32 >> 8)
    }

    pub fn nearest(self, palette: &[Color]) -> Option<usize>
    {
        palette.iter()
               .enumerate()
               .min_by_key(|(_, color)| {
                   let r = i64::from(self.red) - i64::from(color.red);
                   let g = i64::from(self.green) - i64::from(color.green);
                   let b = i64::from(self.blue) - i64::from(color.blue);
                   r * r + g * g + b * b
               })
               .map(|(index, _)| index)
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn scroll_overlap_preserves_clip_and_returns_exposed_shape()
    {
        let mut pixels = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let mut canvas = Canvas::new(&mut pixels, 6, 2);
        let before = (canvas.origin(), canvas.clip());
        let exposed = scroll_rect(&mut canvas, Rect::new(0, 0, 6, 2), 2, 0).unwrap();
        assert_eq!(canvas.pixels,
                   [0xffffff, 0xffffff, 1, 2, 3, 4, 0xffffff, 0xffffff, 7, 8, 9, 10]);
        assert_eq!(exposed, Region::from_rect(Rect::new(0, 0, 2, 2)));
        assert_eq!((canvas.origin(), canvas.clip()), before);
    }
    #[test]
    fn mono_flood_respects_boundary_and_calc_mask_fills_hole()
    {
        let data = [0, 0x7c, 0x44, 0x44, 0x44, 0x7c, 0, 0];
        let source = Bitmap::Mono { data:      &data,
                                    row_bytes: 1,
                                    bounds:    Rect::new(0, 0, 8, 8) };
        let inside = seed_fill(&source,
                               Point { h: 3,
                                       v: 3 }).unwrap();
        assert_eq!(inside.bytes, [0, 0, 0x38, 0x38, 0x38, 0, 0, 0]);
        let mask = calc_mask(&source).unwrap();
        assert_eq!(mask.bytes, [0, 0x7c, 0x7c, 0x7c, 0x7c, 0x7c, 0, 0]);
    }
    #[test]
    fn deep_mask_mixes_components_and_rejects_truncated_sources()
    {
        let source_pixels = [0xff0000, 0x00ff00];
        let mask_pixels = [0x000000, 0x808080];
        let source = Bitmap::Rgb32 { pixels:     &source_pixels,
                                     row_pixels: 2,
                                     bounds:     Rect::new(0, 0, 2, 1) };
        let mask = Bitmap::Rgb32 { pixels:     &mask_pixels,
                                   row_pixels: 2,
                                   bounds:     Rect::new(0, 0, 2, 1) };
        let mut pixels = [0x0000ff; 2];
        let mut canvas = Canvas::new(&mut pixels, 2, 1);
        let bounds = Rect::new(0, 0, 2, 1);
        copy_mask(&mut canvas, &source, bounds, &mask, bounds, bounds, 0, 0xffffff).unwrap();
        assert_eq!(canvas.pixels, [0xff0000, 0x007f80]);
        let palette = [Color::from_rgb8(0), Color::from_rgb8(0xffffff)];
        assert_eq!(Color::from_rgb8(0xeeeeee).nearest(&palette), Some(1));
    }
}
