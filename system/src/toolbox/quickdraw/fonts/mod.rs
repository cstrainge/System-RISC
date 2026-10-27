//! Resident classic Font Manager: resource families, bitmap strikes and sfnt outlines.
//!
//! Original bitmap strikes retain their integer metrics and pixels. Outline
//! rasterization uses checked ttf-parser tables and Fontations' embedded
//! TrueType interpreter with a monochrome target. Pair kerning and complex
//! shaping are separate from this classic glyph-at-a-time service.
#![forbid(unsafe_code)]

use alloc::{string::String, vec::Vec};

use mac_abi::{BAD_ARGUMENT, NO_MEMORY, NOT_FOUND, NOT_SUPPORTED, Status};
use mac_formats::resource::Resource;

use super::{Canvas, Point, font::BitmapFont};

mod bitmap;
mod outline;
mod hinted;
mod monochrome;
mod scratch;
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
    resource: usize,
    validity: FaceValidity
}

#[derive(Clone, Copy, Debug)]
enum FaceValidity
{
    Valid,
    Unsupported,
    Malformed
}
impl FaceValidity
{
    fn inspect(resource: &Resource, size: i16) -> Self
    {
        if size != 0
        {
            return if BitmapFont::parse(&resource.data).is_some()
            {
                Self::Valid
            }
            else
            {
                Self::Malformed
            };
        }
        // Respect preferred outline precedence, including VARC. Only the audited
        // embedded-glyf path is currently admitted; never substitute its base.
        use skrifa::{MetadataProvider, outline::OutlineGlyphFormat};
        let Ok(raw) = skrifa::FontRef::new(&resource.data)
        else
        {
            return Self::Malformed;
        };
        match raw.outline_glyphs().format()
        {
            Some(OutlineGlyphFormat::Glyf) =>
            {
                if ttf_parser::Face::parse(&resource.data, 0).is_ok()
                {
                    Self::Valid
                }
                else
                {
                    Self::Malformed
                }
            }
            Some(OutlineGlyphFormat::Varc | OutlineGlyphFormat::Cff | OutlineGlyphFormat::Cff2) =>
            {
                Self::Unsupported
            }
            None => Self::Malformed
        }
    }
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

/// Use the shared scalar table with exact, fallibly admitted owned UTF-8 backing.
fn catalog_name(bytes: &[u8]) -> Result<String, Status>
{
    let length = bytes.iter()
                      .try_fold(0usize, |length, &byte| {
                          length.checked_add(mac_formats::text::character(byte).len_utf8())
                      })
                      .ok_or(NO_MEMORY)?;
    let mut name = String::new();
    name.try_reserve_exact(length).map_err(|_| NO_MEMORY)?;
    for &byte in bytes
    {
        name.push(mac_formats::text::character(byte));
    }
    Ok(name)
}

impl FontManager
{
    /// Admit the complete catalog before moving font payloads out of System's
    /// decoded fork. Refusal leaves the caller's resources and ordering intact.
    /// Malformed FOND records are omitted; invalid faces fail selection safely.
    pub fn take_resources(resources: &mut Vec<Resource>) -> Result<Self, Status>
    {
        let is_font =
            |resource: &Resource| matches!(&resource.kind, b"FOND" | b"FONT" | b"NFNT" | b"sfnt");
        let font_count = resources.iter().filter(|resource| is_font(resource)).count();
        let mut manager = Self::default();
        manager.resources.try_reserve_exact(font_count).map_err(|_| NO_MEMORY)?;
        // Catalog indexes refer to the same stable filtered order that the
        // final extraction uses, without cloning or moving any payload early.
        for resource in resources.iter().filter(|resource| is_font(resource))
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
            if manager.families.iter().any(|family| family.id == id)
            {
                continue;
            }
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
            for record in data[54..54 + count * 6].chunks_exact(6)
            {
                let size = i16::from_be_bytes([record[0], record[1]]);
                let face = u16::from_be_bytes([record[2], record[3]]);
                let id = i16::from_be_bytes([record[4], record[5]]);
                if size < 0 || face & !0x7f != 0
                {
                    continue;
                }
                // A suitcase's NFNT overrides a legacy ROM FONT at the same ID.
                // Resource map ordering must not choose a different strike.
                let find = |kind| {
                    resources.iter()
                             .filter(|resource| is_font(resource))
                             .enumerate()
                             .find(|(_, resource)| resource.id == id && resource.kind == kind)
                };
                let target = if size == 0
                {
                    find(*b"sfnt")
                }
                else
                {
                    find(*b"NFNT").or_else(|| find(*b"FONT"))
                };
                if let Some((index, resource)) = target
                {
                    let validity = FaceValidity::inspect(resource, size);
                    faces.try_reserve(1).map_err(|_| NO_MEMORY)?;
                    faces.push(Association { size,
                                             face,
                                             resource: index,
                                             validity });
                }
            }
            if faces.is_empty()
            {
                continue;
            }
            let name = match resource.name.as_deref()
            {
                Some(bytes) => catalog_name(bytes)?,
                None =>
                {
                    use core::fmt::Write;
                    let mut name = String::new();
                    // "Font -32768" is the longest possible i16 fallback.
                    name.try_reserve_exact(11).map_err(|_| NO_MEMORY)?;
                    write!(&mut name, "Font {id}").map_err(|_| BAD_ARGUMENT)?;
                    name
                }
            };
            manager.families.try_reserve(1).map_err(|_| NO_MEMORY)?;
            manager.families.push(FontFamily { id,
                                               name,
                                               faces });
        }
        // Family IDs are unique, so the in-place unstable order is identical.
        manager.families.sort_unstable_by_key(|family| family.id);
        // All fallible work is complete. Extraction moves fonts exactly once,
        // preserving the ordinary resources' order and their original capacity.
        for resource in resources.extract_if(.., |resource| is_font(resource))
        {
            manager.resources.push(resource);
        }
        Ok(manager)
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
        // Immutable source validation is cached at catalog admission, not
        // repeated for every text-width request or drawing snapshot.
        match association.validity
        {
            FaceValidity::Valid =>
            {}
            FaceValidity::Unsupported => return Err(NOT_SUPPORTED),
            FaceValidity::Malformed => return Err(BAD_ARGUMENT)
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

    pub fn is_monospaced(&self, selection: FontSelection) -> Result<bool, Status>
    {
        let face = self.resolve(selection, FontPreference::default())?;
        if face.size == 0
        {
            outline::is_monospaced(&face)
        }
        else
        {
            Ok(super::font::BitmapFont::parse(&face.resource.data).ok_or(BAD_ARGUMENT)?
                                                                  .is_monospaced())
        }
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
        if text.is_empty()
        {
            return Ok(0);
        }
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
        if text.is_empty()
        {
            return Ok(0);
        }
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
                      -> Result<(), Status>
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
        scratch::push(spans, (left.saturating_sub(1), right.saturating_add(2)))?;
    }
    Ok(())
}
