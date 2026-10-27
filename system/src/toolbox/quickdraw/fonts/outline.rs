use ab_glyph_rasterizer::{Point as RasterPoint, Rasterizer, point};
use mac_abi::{BAD_ARGUMENT, Status};
use ttf_parser::{Face, GlyphId, OutlineBuilder, PlatformId};

use super::{Canvas, FontMetrics, Point, Resolved, advance, styled_pixel, underline};

fn parse<'a>(face: &Resolved<'a>) -> Result<Face<'a>, Status>
{
    Face::parse(&face.resource.data, 0).map_err(|_| BAD_ARGUMENT)
}

pub(super) fn glyph(face: &Face<'_>, character: char) -> GlyphId
{
    // Modern Unicode cmap subtables take precedence. Original classic sfnt
    // resources also carry platform1/Roman format0 tables; these map MacRoman
    // byte codes, not Unicode scalar values.
    if let Some(id) = face.glyph_index(character)
    {
        return id;
    }
    if let Some(code) = super::super::font::roman(character)
       && let Some(cmap) = face.tables().cmap
    {
        for table in cmap.subtables
        {
            if table.platform_id == PlatformId::Macintosh
               && table.encoding_id == 0
               && let Some(id) = table.glyph_index(u32::from(code))
            {
                return id;
            }
        }
    }
    GlyphId(0)
}

pub(super) fn is_monospaced(resolved: &Resolved<'_>) -> Result<bool, Status>
{
    let face = parse(resolved)?;
    if face.is_monospaced()
    {
        return Ok(true);
    }
    let mut width = None;
    for code in 32u8..=126
    {
        let advance = face.glyph_hor_advance(glyph(&face, char::from(code))).unwrap_or(0);
        if advance == 0
        {
            continue;
        }
        if width.is_some_and(|old| old != advance)
        {
            return Ok(false);
        }
        width = Some(advance);
    }
    Ok(width.is_some())
}

pub(super) fn metrics(resolved: &Resolved<'_>) -> Result<FontMetrics, Status>
{
    let face = parse(resolved)?;
    let scale = f32::from(resolved.selection.size) / f32::from(face.units_per_em());
    let max = face.raw_face()
                  .table(ttf_parser::Tag::from_bytes(b"hhea"))
                  .and_then(|table| super::word(table, 10))
                  .unwrap_or(face.units_per_em());
    // Classic signed fixed rounding adds 0x8000 before taking the high word:
    // -4.5 rounds to -4, while +13.5 rounds to +14.
    Ok(FontMetrics { ascent: (libm::floorf(f32::from(face.ascender())*scale+0.5).max(0.0) as i16)
                         + i16::from(resolved.synthetic & (super::OUTLINE | super::SHADOW) != 0),
                     descent: ((-libm::floorf(f32::from(face.descender())*scale+0.5)).max(0.0) as i16)
                         + if resolved.synthetic & super::SHADOW != 0 {2} else {i16::from(resolved.synthetic & super::OUTLINE != 0)},
                     leading: libm::floorf(f32::from(face.line_gap())*scale+0.5).max(0.0) as i16,
                     max_width: advance(libm::ceilf(f32::from(max)*scale),resolved.synthetic).clamp(0,i32::from(i16::MAX)) as i16 })
}

pub(super) fn width(resolved: &Resolved<'_>, text: &str) -> Result<i16, Status>
{
    if resolved.hinted
    {
        return super::hinted::width(resolved, text);
    }
    let face = parse(resolved)?;
    let scale = f32::from(resolved.selection.size) / f32::from(face.units_per_em());
    let mut width = 0i32;
    for character in text.chars()
    {
        width = width.saturating_add(advance(f32::from(face.glyph_hor_advance(glyph(&face,
                                                                                    character))
                                                           .unwrap_or(0))
                                             * scale,
                                             resolved.synthetic));
    }
    Ok(width.clamp(0, i32::from(i16::MAX)) as i16)
}

struct Builder<'a>
{
    raster:   &'a mut Rasterizer,
    scale_x:  f32,
    scale_y:  f32,
    shear:    f32,
    left:     f32,
    top:      f32,
    first:    RasterPoint,
    previous: RasterPoint
}

impl Builder<'_>
{
    fn point(&self, x: f32, y: f32) -> RasterPoint
    {
        point(x * self.scale_x - y * self.scale_y * self.shear - self.left,
              -y * self.scale_y - self.top)
    }
}

impl OutlineBuilder for Builder<'_>
{
    fn move_to(&mut self, x: f32, y: f32)
    {
        self.previous = self.point(x, y);
        self.first = self.previous;
    }

    fn line_to(&mut self, x: f32, y: f32)
    {
        let next = self.point(x, y);
        self.raster.draw_line(self.previous, next);
        self.previous = next;
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32)
    {
        let next = self.point(x, y);
        self.raster.draw_quad(self.previous, self.point(x1, y1), next);
        self.previous = next;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32)
    {
        let next = self.point(x, y);
        self.raster.draw_cubic(self.previous, self.point(x1, y1), self.point(x2, y2), next);
        self.previous = next;
    }

    fn close(&mut self)
    {
        self.raster.draw_line(self.previous, self.first);
        self.previous = self.first;
    }
}

pub(super) fn draw(resolved: &Resolved<'_>, canvas: &mut Canvas<'_>, baseline: Point, text: &str,
                   color: u32)
                   -> Result<i16, Status>
{
    if resolved.hinted
    {
        return super::hinted::draw(resolved, canvas, baseline, text, color);
    }
    let face = parse(resolved)?;
    let scale_y = f32::from(resolved.selection.size) / f32::from(face.units_per_em());
    let scale_x = scale_y;
    let shear = if resolved.synthetic & super::ITALIC != 0 { 0.2 } else { 0.0 };
    let mut pen = i32::from(baseline.h);
    let mut descenders = alloc::vec::Vec::new();
    for character in text.chars()
    {
        let id = glyph(&face, character);
        if let Some(bounds) = face.glyph_bounding_box(id)
        {
            let x0 = f32::from(bounds.x_min) * scale_x;
            let x1 = f32::from(bounds.x_max) * scale_x;
            let y0 = -f32::from(bounds.y_max) * scale_y;
            let y1 = -f32::from(bounds.y_min) * scale_y;
            let left = libm::floorf((x0 + y0 * shear).min(x0 + y1 * shear)) as i32;
            let right = libm::ceilf((x1 + y0 * shear).max(x1 + y1 * shear)) as i32;
            let top = libm::floorf(y0) as i32;
            let bottom = libm::ceilf(y1) as i32;
            let width = usize::try_from(right - left).map_err(|_| BAD_ARGUMENT)?;
            let height = usize::try_from(bottom - top).map_err(|_| BAD_ARGUMENT)?;
            if width > 2048
               || height > 2048
               || width.checked_mul(height).is_none_or(|area| area > 1024 * 1024)
            {
                return Err(BAD_ARGUMENT);
            }
            if width > 0 && height > 0
            {
                let length =
                    width.checked_mul(height).and_then(|n| n.checked_add(4)).ok_or(BAD_ARGUMENT)?;
                let buffer = super::super::storage::vector(length).map_err(|_| mac_abi::NO_MEMORY)?;
                let mut raster =
                    Rasterizer::with_buffer(width, height, buffer).map_err(|_| BAD_ARGUMENT)?;
                let mut builder = Builder { raster: &mut raster,
                                            scale_x,
                                            scale_y,
                                            shear,
                                            left: left as f32,
                                            top: top as f32,
                                            first: point(0.0, 0.0),
                                            previous: point(0.0, 0.0) };
                // A malformed outline may emit a partial path. Discard the
                // complete raster unless ttf-parser validates the whole glyph.
                if face.outline_glyph(id, &mut builder).is_some()
                {
                    let mut coverage = super::scratch::filled(width * height, false)?;
                    raster.for_each_pixel(|index, alpha| coverage[index] = alpha >= 0.5);
                    let gx = pen.saturating_add(left);
                    let gy = i32::from(baseline.v).saturating_add(top);
                    if resolved.synthetic & super::UNDERLINE != 0
                    {
                        super::remember_descender(&mut descenders,
                                                  coverage.iter()
                                                          .enumerate()
                                                          .filter(|&(_, ink)| *ink)
                                                          .map(|(index, _)| {
                                                              (gx + (index % width) as i32,
                                                               gy + (index / width) as i32)
                                                          }),
                                                  i32::from(baseline.v))?;
                    }
                    let mask = |x: i32, y: i32| {
                        let x = x - gx;
                        let y = y - gy;
                        x >= 0
                        && y >= 0
                        && (x as usize) < width
                        && (y as usize) < height
                        && coverage[y as usize * width + x as usize]
                    };
                    for y in 0..height
                    {
                        for x in 0..width
                        {
                            if coverage[y * width + x]
                            {
                                styled_pixel(canvas,
                                             gx + x as i32,
                                             gy + y as i32,
                                             resolved.synthetic,
                                             color,
                                             &mask);
                            }
                        }
                    }
                }
            }
        }
        pen = pen.saturating_add(advance(f32::from(face.glyph_hor_advance(id).unwrap_or(0))
                                         * scale_y,
                                         resolved.synthetic));
    }
    let width = pen.saturating_sub(i32::from(baseline.h)).clamp(0, i32::from(i16::MAX)) as i16;
    underline(canvas, baseline, width, resolved.synthetic, color, &descenders);
    Ok(width)
}
