//! Original sfnt instructions executed by Fontations' safe TrueType interpreter.
use alloc::vec::Vec;

use ab_glyph_rasterizer::point;
use mac_abi::{BAD_ARGUMENT, Status};
use skrifa::{FontRef, MetadataProvider,
             instance::{LocationRef, Size},
             outline::{DrawSettings, Engine, HintingInstance, HintingOptions, Target,
                       pen::{NullPen, PathElement}}};

use super::{Canvas, Point, Resolved, advance, styled_pixel, underline};

fn hint(outlines: &skrifa::OutlineGlyphCollection<'_>, size: i16)
        -> Result<HintingInstance, Status>
{
    HintingInstance::new(outlines,
                         Size::new(f32::from(size)),
                         LocationRef::default(),
                         HintingOptions { engine: Engine::Interpreter,
                                          target: Target::Mono }).map_err(|_| BAD_ARGUMENT)
}

pub(super) fn width(resolved: &Resolved<'_>, text: &str) -> Result<i16, Status>
{
    let raw = FontRef::new(&resolved.resource.data).map_err(|_| BAD_ARGUMENT)?;
    let face = ttf_parser::Face::parse(&resolved.resource.data, 0).map_err(|_| BAD_ARGUMENT)?;
    let outlines = raw.outline_glyphs();
    let hinter = hint(&outlines, resolved.selection.size)?;
    let mut width = 0i32;
    for character in text.chars()
    {
        let id = super::outline::glyph(&face, character);
        let glyph = outlines.get(skrifa::GlyphId::new(u32::from(id.0))).ok_or(BAD_ARGUMENT)?;
        let metrics = glyph.draw(DrawSettings::hinted(&hinter, false), &mut NullPen)
                           .map_err(|_| BAD_ARGUMENT)?;
        let step = metrics.advance_width.unwrap_or_else(|| {
                                            f32::from(face.glyph_hor_advance(id).unwrap_or(0))
                                            * f32::from(resolved.selection.size)
                                            / f32::from(face.units_per_em())
                                        });
        width = width.saturating_add(advance(step, resolved.synthetic));
    }
    Ok(width.clamp(0, i32::from(i16::MAX)) as i16)
}

pub(super) fn draw(resolved: &Resolved<'_>, canvas: &mut Canvas<'_>, baseline: Point, text: &str,
                   color: u32)
                   -> Result<i16, Status>
{
    let raw = FontRef::new(&resolved.resource.data).map_err(|_| BAD_ARGUMENT)?;
    let face = ttf_parser::Face::parse(&resolved.resource.data, 0).map_err(|_| BAD_ARGUMENT)?;
    let outlines = raw.outline_glyphs();
    let hinter = hint(&outlines, resolved.selection.size)?;
    let shear = if resolved.synthetic & super::ITALIC != 0 { 0.2 } else { 0.0 };
    let mut pen = i32::from(baseline.h);
    let mut descenders = Vec::new();
    for character in text.chars()
    {
        let id = super::outline::glyph(&face, character);
        let glyph = outlines.get(skrifa::GlyphId::new(u32::from(id.0))).ok_or(BAD_ARGUMENT)?;
        let mut path = Vec::<PathElement>::new();
        let metrics = glyph.draw(DrawSettings::hinted(&hinter, false), &mut path)
                           .map_err(|_| BAD_ARGUMENT)?;
        if path.len() > 131072
        {
            return Err(BAD_ARGUMENT);
        }
        let mut left = f32::INFINITY;
        let mut top = f32::INFINITY;
        let mut right = f32::NEG_INFINITY;
        let mut bottom = f32::NEG_INFINITY;
        let transform = |x: f32, y: f32| point(x - y * shear, -y);
        let mut bounds = |x, y| {
            let p = transform(x, y);
            left = left.min(p.x);
            right = right.max(p.x);
            top = top.min(p.y);
            bottom = bottom.max(p.y);
        };
        for element in &path
        {
            match *element
            {
                PathElement::MoveTo { x,
                                      y }
                | PathElement::LineTo { x,
                                      y } => bounds(x, y),
                PathElement::QuadTo { cx0,
                                      cy0,
                                      x,
                                      y } =>
                {
                    bounds(cx0, cy0);
                    bounds(x, y);
                }
                PathElement::CurveTo { cx0,
                                       cy0,
                                       cx1,
                                       cy1,
                                       x,
                                       y } =>
                {
                    bounds(cx0, cy0);
                    bounds(cx1, cy1);
                    bounds(x, y);
                }
                PathElement::Close =>
                {}
            }
        }
        if left.is_finite() && right > left && bottom > top
        {
            let left = libm::floorf(left) as i32;
            let top = libm::floorf(top) as i32;
            let width =
                usize::try_from(libm::ceilf(right) as i32 - left).map_err(|_| BAD_ARGUMENT)?;
            let height =
                usize::try_from(libm::ceilf(bottom) as i32 - top).map_err(|_| BAD_ARGUMENT)?;
            if width > 2048
               || height > 2048
               || width.checked_mul(height).is_none_or(|area| area > 1024 * 1024)
            {
                return Err(BAD_ARGUMENT);
            }
            let local = |x, y| {
                let p = transform(x, y);
                point(p.x - left as f32, p.y - top as f32)
            };
            let coverage = super::monochrome::rasterize(&path, width, height, &local);
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
                                          i32::from(baseline.v));
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
        let step = metrics.advance_width.unwrap_or_else(|| {
                                            f32::from(face.glyph_hor_advance(id).unwrap_or(0))
                                            * f32::from(resolved.selection.size)
                                            / f32::from(face.units_per_em())
                                        });
        pen = pen.saturating_add(advance(step, resolved.synthetic));
    }
    let width = pen.saturating_sub(i32::from(baseline.h)).clamp(0, i32::from(i16::MAX)) as i16;
    underline(canvas, baseline, width, resolved.synthetic, color, &descenders);
    Ok(width)
}
