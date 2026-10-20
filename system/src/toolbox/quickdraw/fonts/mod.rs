//! Resident classic Font Manager: resource families, bitmap strikes and sfnt outlines.
//!
//! Original bitmap strikes retain their integer metrics and pixels. Outline
//! rasterization uses checked ttf-parser tables and Fontations' embedded
//! TrueType interpreter with a monochrome target. Pair kerning and complex
//! shaping are separate from this classic glyph-at-a-time service.
#![forbid(unsafe_code)]

use alloc::{string::String, vec::Vec};

use mac_abi::{BAD_ARGUMENT, NOT_FOUND, Status};
use mac_formats::resource::Resource;

use super::{Canvas, Point, font::BitmapFont};

mod bitmap;
mod outline;
mod hinted;
mod monochrome;
#[cfg(test)]
mod tests;

pub const BOLD: u16 = 1;
pub const ITALIC: u16 = 2;
pub const UNDERLINE: u16 = 4;
pub const OUTLINE: u16 = 8;
pub const SHADOW: u16 = 16;
pub const CONDENSED: u16 = 32;
pub const EXTENDED: u16 = 64;
pub const MAX_POINT_SIZE: i16 = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontSelection
{
    pub family: i16,
    pub size:   i16,
    pub face:   u16
}

impl Default for FontSelection
{
    fn default() -> Self
    {
        Self { family: 0,
               size:   12,
               face:   0 }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FontMetrics
{
    pub ascent:    i16,
    pub descent:   i16,
    pub leading:   i16,
    pub max_width: i16
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FontPreference
{
    #[default]
    BitmapPreferred,
    OutlinePreferred,
    /// Diagnostic comparison path without executing embedded hint programs.
    OutlineUnhinted
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontSource
{
    Bitmap
    {
        kind: [u8; 4], id: i16, size: i16, face: u16
    },
    TrueType
    {
        id: i16, face: u16
    }
}

#[derive(Clone, Debug)]
pub struct FontFamily
{
    pub id:   i16,
    pub name: String,
    faces:    Vec<Association>
}

#[derive(Clone, Copy, Debug)]
struct Association
{
    size:     i16,
    face:     u16,
    resource: usize
}

#[derive(Default)]
pub struct FontManager
{
    resources: Vec<Resource>,
    families:  Vec<FontFamily>
}

struct Resolved<'a>
{
    resource:  &'a Resource,
    size:      i16,
    selection: FontSelection,
    synthetic: u16,
    face:      u16,
    hinted:    bool
}

fn word(data: &[u8], offset: usize) -> Option<u16>
{
    Some(u16::from_be_bytes(data.get(offset..offset.checked_add(2)?)?.try_into().ok()?))
}

impl FontManager
{
    /// Move font resources out of System's decoded fork, retaining one owned
    /// copy of every payload. Applications keep only family/size/style values.
    /// Malformed FOND records are omitted; invalid faces fail selection safely.
    pub fn take_resources(resources: &mut Vec<Resource>) -> Self
    {
        let mut manager = Self::default();
        for resource in core::mem::take(resources)
        {
            if matches!(&resource.kind, b"FOND" | b"FONT" | b"NFNT" | b"sfnt")
            {
                manager.resources.push(resource);
            }
            else
            {
                resources.push(resource);
            }
        }
        for resource in &manager.resources
        {
            if resource.kind != *b"FOND"
            {
                continue;
            }
            let data = &resource.data;
            let Some(id) = word(data, 2).map(|id| id as i16)
            else
            {
                continue;
            };
            let Some(count) = word(data, 52).map(|n| usize::from(n) + 1)
            else
            {
                continue;
            };
            if count > 4096 || 54usize.checked_add(count * 6).is_none_or(|end| end > data.len())
            {
                continue;
            }
            let mut faces = Vec::new();
            for index in 0..count
            {
                let offset = 54 + index * 6;
                let size = word(data, offset).unwrap() as i16;
                let face = word(data, offset + 2).unwrap();
                let id = word(data, offset + 4).unwrap() as i16;
                // A suitcase's NFNT overrides a legacy ROM FONT at the same ID.
                // Resource map ordering must not choose a different strike.
                let find =
                    |kind| manager.resources.iter().position(|r| r.id == id && r.kind == kind);
                let target = if size == 0
                {
                    find(*b"sfnt")
                }
                else
                {
                    find(*b"NFNT").or_else(|| find(*b"FONT"))
                };
                if size >= 0
                   && face & !0x7f == 0
                   && let Some(resource) = target
                {
                    faces.push(Association { size,
                                             face,
                                             resource });
                }
            }
            let name = resource.name
                               .as_deref()
                               .map(mac_formats::text::decode)
                               .unwrap_or_else(|| alloc::format!("Font {id}"));
            if !faces.is_empty() && !manager.families.iter().any(|family| family.id == id)
            {
                manager.families.push(FontFamily { id,
                                                   name,
                                                   faces });
            }
        }
        manager.families.sort_by_key(|family| family.id);
        manager
    }

    pub fn resources(&self) -> &[Resource]
    {
        &self.resources
    }

    pub fn catalog(&self) -> &[FontFamily]
    {
        &self.families
    }

    pub fn family_id(&self, name: &str) -> Option<i16>
    {
        self.families.iter().find(|family| family.name.eq_ignore_ascii_case(name)).map(|f| f.id)
    }

    pub fn family_name(&self, id: i16) -> Option<&str>
    {
        let id = if id == 1 { 3 } else { id };
        self.families.iter().find(|family| family.id == id).map(|family| family.name.as_str())
    }

    fn resolve(&self, mut selection: FontSelection, preference: FontPreference)
               -> Result<Resolved<'_>, Status>
    {
        if selection.size < 0 || selection.size > MAX_POINT_SIZE || selection.face & !0x7f != 0
        {
            return Err(BAD_ARGUMENT);
        }
        if selection.family == 1
        {
            selection.family = 3;
        }
        if selection.size == 0
        {
            selection.size = 12;
        }
        let family = self.families.iter().find(|f| f.id == selection.family).ok_or(NOT_FOUND)?;
        let wanted = selection.face & (BOLD | ITALIC);
        let score = |face: &Association| {
            let different = (face.face ^ wanted).count_ones();
            let wrong_size = (i32::from(face.size) - i32::from(selection.size)).unsigned_abs();
            // An actual matching bold/italic face takes priority over synthesis.
            // Exact bitmap strikes retain classic pixels unless outlines are requested.
            let format = if face.size == 0
            {
                if preference != FontPreference::BitmapPreferred { 0 } else { 1 }
            }
            else if face.size == selection.size
            {
                if preference == FontPreference::BitmapPreferred { 0 } else { 1 }
            }
            else
            {
                2
            };
            (different, format, if face.size == 0 { 0 } else { wrong_size })
        };
        let association = family.faces.iter().min_by_key(|face| score(face)).ok_or(NOT_FOUND)?;
        let resource = &self.resources[association.resource];
        if association.size == 0
        {
            ttf_parser::Face::parse(&resource.data, 0).map_err(|_| BAD_ARGUMENT)?;
        }
        else if BitmapFont::parse(&resource.data).is_none()
        {
            return Err(BAD_ARGUMENT);
        }
        Ok(Resolved { resource,
                      size: association.size,
                      selection,
                      synthetic: selection.face & !association.face,
                      face: association.face,
                      hinted: preference != FontPreference::OutlineUnhinted })
    }

    pub fn source(&self, selection: FontSelection, preference: FontPreference)
                  -> Result<FontSource, Status>
    {
        let face = self.resolve(selection, preference)?;
        Ok(if face.size == 0
           {
               FontSource::TrueType { id:   face.resource.id,
                                      face: face.face }
           }
           else
           {
               FontSource::Bitmap { kind: face.resource.kind,
                                    id:   face.resource.id,
                                    size: face.size,
                                    face: face.face }
           })
    }

    pub fn metrics(&self, selection: FontSelection) -> Result<FontMetrics, Status>
    {
        self.metrics_with_preference(selection, FontPreference::BitmapPreferred)
    }

    /// Conservative ink extent for retained bitmap text, cached with the draw
    /// command. Outline/hinted text retains its ordinary clipped drawing path.
    #[cfg_attr(not(target_arch = "riscv64"), allow(dead_code))]
    pub(crate) fn vertical_ink_bounds(&self, selection: FontSelection)
                                      -> Result<Option<(i32, i32)>, Status>
    {
        let face = self.resolve(selection, FontPreference::BitmapPreferred)?;
        if face.size == 0
        {
            return Ok(None);
        }
        let font =
            super::font::BitmapFont::parse(&face.resource.data).ok_or(mac_abi::BAD_ARGUMENT)?;
        let (top, bottom) = font.vertical_ink_bounds();
        let scale = f32::from(face.selection.size) / f32::from(face.size);
        // Outline/shadow synthesis extends ink by at most two pixels; the
        // underline is at baseline+1. Include these without trusting line metrics.
        Ok(Some(((libm::floorf(top as f32 * scale) as i32).min(0) - 2,
                 (libm::ceilf(bottom as f32 * scale) as i32).max(2) + 2)))
    }

    pub fn metrics_with_preference(&self, selection: FontSelection, preference: FontPreference)
                                   -> Result<FontMetrics, Status>
    {
        let face = self.resolve(selection, preference)?;
        if face.size == 0 { outline::metrics(&face) } else { bitmap::metrics(&face) }
    }

    pub fn text_width(&self, selection: FontSelection, text: &str) -> Result<i16, Status>
    {
        self.text_width_with_preference(selection, text, FontPreference::BitmapPreferred)
    }

    pub fn text_width_with_preference(&self, selection: FontSelection, text: &str,
                                      preference: FontPreference)
                                      -> Result<i16, Status>
    {
        let face = self.resolve(selection, preference)?;
        if face.size == 0 { outline::width(&face, text) } else { bitmap::width(&face, text) }
    }

    pub fn draw(&self, selection: FontSelection, canvas: &mut Canvas<'_>, baseline: Point,
                text: &str, color: u32)
                -> Result<i16, Status>
    {
        self.draw_with_preference(selection,
                                  canvas,
                                  baseline,
                                  text,
                                  color,
                                  FontPreference::BitmapPreferred)
    }

    pub fn draw_with_preference(&self, selection: FontSelection, canvas: &mut Canvas<'_>,
                                baseline: Point, text: &str, color: u32,
                                preference: FontPreference)
                                -> Result<i16, Status>
    {
        let face = self.resolve(selection, preference)?;
        if face.size == 0
        {
            outline::draw(&face, canvas, baseline, text, color)
        }
        else
        {
            bitmap::draw(&face, canvas, baseline, text, color)
        }
    }
}

fn advance(value: f32, face: u16) -> i32
{
    (libm::roundf(value) as i32
     + i32::from(face & BOLD != 0)
     + i32::from(face & (OUTLINE | SHADOW) != 0)
     + i32::from(face & EXTENDED != 0)
     - i32::from(face & CONDENSED != 0)).max(0)
}

fn pixel(canvas: &mut Canvas<'_>, x: i32, y: i32, color: u32)
{
    if let (Ok(x), Ok(y)) = (i16::try_from(x), i16::try_from(y))
       && x < i16::MAX
       && y < i16::MAX
    {
        canvas.paint_rect(super::Rect::new(x, y, x + 1, y + 1), color);
    }
}

fn styled_pixel(canvas: &mut Canvas<'_>, x: i32, y: i32, face: u16, color: u32,
                mask: &impl Fn(i32, i32) -> bool)
{
    let copies = if face & BOLD != 0 { 2 } else { 1 };
    for offset in 0..copies
    {
        let x = x.saturating_add(offset);
        if face & (OUTLINE | SHADOW) != 0
        {
            // Original outlines use all eight neighboring pixels. Shadow
            // adds a second pixel on the right and below, while preserving
            // the same hollow glyph interior (observed in original glyph H).
            let extent = if face & SHADOW != 0 { 2 } else { 1 };
            for dy in -1..=extent
            {
                for dx in -1..=extent
                {
                    if !mask(x.saturating_add(dx), y.saturating_add(dy))
                    {
                        pixel(canvas, x.saturating_add(dx), y.saturating_add(dy), color);
                    }
                }
            }
        }
        else
        {
            pixel(canvas, x, y, color);
        }
    }
}

fn underline(canvas: &mut Canvas<'_>, start: Point, width: i16, face: u16, color: u32,
             descenders: &[(i32, i32)])
{
    if face & UNDERLINE != 0
    {
        for offset in 0..i32::from(width)
        {
            let x = i32::from(start.h) + offset;
            if !descenders.iter().any(|&(left, right)| x >= left && x < right)
            {
                pixel(canvas, x, i32::from(start.v) + 1, color);
            }
        }
    }
}

fn remember_descender(spans: &mut Vec<(i32, i32)>, pixels: impl Iterator<Item = (i32, i32)>,
                      baseline: i32)
{
    let mut bounds: Option<(i32, i32)> = None;
    for (x, _) in pixels.filter(|&(_, y)| y >= baseline)
    {
        bounds = Some(bounds.map_or((x, x), |(left, right)| (left.min(x), right.max(x))));
    }
    if let Some((left, right)) = bounds
    {
        // Original underline skips the descender's horizontal ink extent,
        // expanded by one pixel on each side, while retaining glyph ink.
        spans.push((left.saturating_sub(1), right.saturating_add(2)));
    }
}
