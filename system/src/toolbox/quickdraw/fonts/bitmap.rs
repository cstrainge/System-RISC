use alloc::vec::Vec;

use mac_abi::{BAD_ARGUMENT, Status};

use super::{Canvas, FontMetrics, Point, Resolved, advance, styled_pixel, underline};
use crate::toolbox::quickdraw::font::BitmapFont;

pub(super) fn metrics(face: &Resolved<'_>) -> Result<FontMetrics, Status>
{
    let font = BitmapFont::parse(&face.resource.data).ok_or(BAD_ARGUMENT)?;
    let metrics = font.metrics();
    let scale = f32::from(face.selection.size) / f32::from(face.size);
    Ok(FontMetrics { ascent: libm::ceilf(f32::from(metrics.ascent) * scale) as i16,
                     descent: libm::ceilf(f32::from(metrics.descent) * scale) as i16,
                     leading: libm::roundf(f32::from(metrics.leading) * scale) as i16,
                     max_width: advance(f32::from(font.maximum_width()) * scale, face.synthetic)
                         .clamp(0, i32::from(i16::MAX)) as i16 })
}

pub(super) fn width(face: &Resolved<'_>, text: &str) -> Result<i16, Status>
{
    let font = BitmapFont::parse(&face.resource.data).ok_or(BAD_ARGUMENT)?;
    let scale = f32::from(face.selection.size) / f32::from(face.size);
    let mut width = 0i32;
    for c in text.chars()
    {
        let mut bytes = [0; 4];
        width = width.saturating_add(advance(f32::from(font.width(c.encode_utf8(&mut bytes)))
                                             * scale,
                                             face.synthetic));
    }
    Ok(width.clamp(0, i32::from(i16::MAX)) as i16)
}

pub(super) fn draw(face: &Resolved<'_>, canvas: &mut Canvas<'_>, baseline: Point, text: &str,
                   color: u32)
                   -> Result<i16, Status>
{
    let font = BitmapFont::parse(&face.resource.data).ok_or(BAD_ARGUMENT)?;
    let scale_y = f32::from(face.selection.size) / f32::from(face.size);
    let scale_x = scale_y;
    let mut pen = i32::from(baseline.h);
    let mut descenders = Vec::new();
    for c in text.chars()
    {
        // Keep exact, unstyled strikes on the original renderer, including its
        // missing-glyph behavior, signed bearings and original baseline.
        if face.synthetic == 0 && face.size == face.selection.size
        {
            let mut bytes = [0; 4];
            let text = c.encode_utf8(&mut bytes);
            if let Ok(x) = i16::try_from(pen)
            {
                font.draw(canvas, x, baseline.v, text, color);
            }
            pen = pen.saturating_add(i32::from(font.width(text)));
            continue;
        }
        let mut pixels = Vec::new();
        let mut error = None;
        let step = font.glyph_ink(c, |x, y| {
                           if error.is_some()
                           {
                               return;
                           }
                           let result = (|| -> Result<(), Status> {
                               let left = libm::floorf(x as f32 * scale_x) as i32;
                               let right = libm::ceilf((x + 1) as f32 * scale_x) as i32;
                               let top = libm::floorf(y as f32 * scale_y) as i32;
                               let bottom = libm::ceilf((y + 1) as f32 * scale_y) as i32;
                               for row in top..bottom
                               {
                                   let shift = if face.synthetic & super::ITALIC != 0
                                   {
                                       libm::roundf(-(row as f32) * 0.2) as i32
                                   }
                                   else
                                   {
                                       0
                                   };
                                   for column in left..right
                                   {
                                       if pixels.len() >= 65536
                                       {
                                           return Err(BAD_ARGUMENT);
                                       }
                                       super::scratch::push(&mut pixels, (column + shift, row))?;
                                   }
                               }
                               Ok(())
                           })();
                           if let Err(cause) = result
                           {
                               error = Some(cause);
                           }
                       });
        if let Some(error) = error
        {
            return Err(error);
        }
        pixels.sort_unstable();
        pixels.dedup();
        if face.synthetic & super::UNDERLINE != 0
        {
            super::remember_descender(&mut descenders,
                                      pixels.iter()
                                            .map(|&(x, y)| (pen + x, i32::from(baseline.v) + y)),
                                      i32::from(baseline.v))?;
        }
        // Style effects are geometric first-cut synthesis. Exact matching styled
        // NFNT or sfnt resources are selected before this fallback is reached.
        for &(x, y) in &pixels
        {
            let gx = pen.saturating_add(x);
            let gy = i32::from(baseline.v).saturating_add(y);
            if face.synthetic & (super::OUTLINE | super::SHADOW) == 0
            {
                styled_pixel(canvas, gx, gy, face.synthetic, color, &|_, _| false);
            }
            else
            {
                styled_pixel(canvas, gx, gy, face.synthetic, color, &|mx, my| {
                    pixels.binary_search(&(mx - pen, my - i32::from(baseline.v))).is_ok()
                });
            }
        }
        pen = pen.saturating_add(advance(f32::from(step) * scale_y, face.synthetic));
    }
    let width = pen.saturating_sub(i32::from(baseline.h)).clamp(0, i32::from(i16::MAX)) as i16;
    underline(canvas, baseline, width, face.synthetic, color, &descenders);
    Ok(width)
}
