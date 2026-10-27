//! Safe decoding and drawing of classic icon families and Finder bundle mappings.
#![forbid(unsafe_code)]
#[cfg(test)]
use alloc::vec;
use alloc::vec::Vec;

use mac_formats::resource::{Resource, ResourceRef};

use super::{Canvas, Rect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconError
{
    NotFound,
    Truncated,
    InvalidData,
    MissingMask,
    Unsupported,
    TooLarge,
    Allocation
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IconFlags
{
    pub selected: bool,
    pub disabled: bool,
    pub open:     bool
}
impl IconFlags
{
    pub fn from_transform(value: u16) -> Result<Self, IconError>
    {
        if value & !0x4003 != 0 || value & 3 == 2
        {
            return Err(IconError::Unsupported);
        }
        Ok(Self { selected: value & 0x4000 != 0,
                  disabled: value & 3 == 1,
                  open:     value & 3 == 3 })
    }
}
/// Framebuffer presentation is separate from logical RGB resource decoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IconDisplay
{
    LogicalRgb,
    #[default]
    System7Indexed
}
impl IconDisplay
{
    fn entry(rgb: u32) -> Option<&'static [u8; 32]>
    {
        // Original 8-bit GDevice and Icon Utilities colors, measured for every
        // clut 4/8 entry. This is not an assumed scalar gamma/darkening curve.
        const TABLE: &[u8] = include_bytes!("../../../assets/icons/system7-indexed-display.bin");
        let entries = TABLE.as_chunks::<32>().0;
        entries.binary_search_by_key(&rgb, |entry| {
                   u32::from_be_bytes(entry[..4].try_into().unwrap())
               })
               .ok()
               .map(|index| &entries[index])
    }

    fn color(self, rgb: u32) -> u32
    {
        if self == Self::LogicalRgb
        {
            return rgb;
        }
        Self::entry(rgb).map_or(rgb, |entry| u32::from_be_bytes(entry[4..8].try_into().unwrap()))
    }

    fn transform(self, rgb: u32, flags: IconFlags) -> Result<u32, IconError>
    {
        if !flags.selected && !flags.disabled
        {
            return Ok(self.color(rgb));
        }
        let entry = Self::entry(rgb).ok_or(IconError::Unsupported)?;
        let transform = match (flags.selected, flags.disabled)
        {
            (true, false) => 0,
            (false, true) => 1,
            (true, true) => 2,
            (false, false) => unreachable!()
        };
        let offset = match self
        {
            Self::LogicalRgb => 8 + transform * 4,
            Self::System7Indexed => 20 + transform * 4
        };
        Ok(u32::from_be_bytes(entry[offset..offset + 4].try_into().unwrap()))
    }
}

#[derive(Clone, Debug)]
pub struct IconPalettes
{
    pub four:  [u32; 16],
    pub eight: [u32; 256]
}
impl Default for IconPalettes
{
    fn default() -> Self
    {
        // Original Macintosh ROM clut 4; supplied resource tables override this.
        let four = [0xffffff, 0xfcf305, 0xff6402, 0xdd0806, 0xf20884, 0x4600a5, 0x0000d4,
                    0x02abea, 0x1fb714, 0x006411, 0x562c05, 0x90713a, 0xc0c0c0, 0x808080,
                    0x404040, 0];
        let mut eight = [0; 256];
        let mut cursor = 0;
        for r in [255, 204, 153, 102, 51, 0]
        {
            for g in [255, 204, 153, 102, 51, 0]
            {
                for b in [255, 204, 153, 102, 51, 0]
                {
                    if r != 0 || g != 0 || b != 0
                    {
                        eight[cursor] = (r << 16) | (g << 8) | b;
                        cursor += 1;
                    }
                }
            }
        }
        for channel in [16, 8, 0]
        {
            for value in [238, 221, 187, 170, 136, 119, 85, 68, 34, 17]
            {
                eight[cursor] = value << channel;
                cursor += 1;
            }
        }
        for value in [238, 221, 187, 170, 136, 119, 85, 68, 34, 17]
        {
            eight[cursor] = value * 0x010101;
            cursor += 1;
        }
        eight[cursor] = 0;
        Self { four,
               eight }
    }
}
impl IconPalettes
{
    pub fn from_resource_refs(resources: &[ResourceRef<'_>]) -> Result<Self, IconError>
    {
        let mut result = Self::default();
        for (id, target) in [(4, &mut result.four[..]), (8, &mut result.eight[..])]
        {
            if let Some(resource) = find(resources, *b"clut", id)
            {
                let table = color_table(resource.data)?;
                for (index, color) in table
                {
                    let slot = target.get_mut(index).ok_or(IconError::InvalidData)?;
                    *slot = color;
                }
            }
        }
        Ok(result)
    }
}
#[derive(Clone, Debug)]
pub struct IconImage
{
    pub width:  u16,
    pub height: u16,
    pub depth:  u8,
    pub pixels: Vec<u32>,
    pub mask:   Vec<u8>
}
#[derive(Clone, Debug)]
pub struct IconFamily
{
    pub id:     i16,
    pub images: Vec<IconImage>
}
fn find<'a>(resources: &[ResourceRef<'a>], kind: [u8; 4], id: i16) -> Option<ResourceRef<'a>>
{
    resources.iter().find(|r| r.kind == kind && r.id == id).copied()
}
fn refs(resources: &[Resource]) -> Result<Vec<ResourceRef<'_>>, IconError>
{
    let mut result = allocated(resources.len())?;
    result.extend(resources.iter().map(|r| {
                                      ResourceRef { kind:       r.kind,
                                                    id:         r.id,
                                                    name:       r.name.as_deref(),
                                                    attributes: r.attributes,
                                                    data:       &r.data }
                                  }));
    Ok(result)
}
fn allocated<T>(count: usize) -> Result<Vec<T>, IconError>
{
    let mut values = Vec::new();
    values.try_reserve_exact(count).map_err(|_| IconError::Allocation)?;
    Ok(values)
}
fn word(data: &[u8], offset: usize) -> Result<u16, IconError>
{
    let bytes = data.get(offset..offset + 2).ok_or(IconError::Truncated)?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}
fn rect(data: &[u8], offset: usize) -> Result<Rect, IconError>
{
    Ok(Rect::from_be_bytes(data.get(offset..offset + 8)
                               .ok_or(IconError::Truncated)?
                               .try_into()
                               .unwrap()))
}
fn color_table(data: &[u8]) -> Result<Vec<(usize, u32)>, IconError>
{
    let count = usize::from(word(data, 6)?) + 1;
    if count > 256
    {
        return Err(IconError::InvalidData);
    }
    let flags = word(data, 4)?;
    data.get(8..8 + count * 8).ok_or(IconError::Truncated)?;
    let mut result = allocated(count)?;
    for i in 0..count
    {
        let at = 8 + i * 8;
        let index = if flags & 0x8000 != 0 { i } else { usize::from(word(data, at)?) };
        let color = (u32::from(word(data, at + 2)? >> 8) << 16)
                    | (u32::from(word(data, at + 4)? >> 8) << 8)
                    | u32::from(word(data, at + 6)? >> 8);
        result.push((index, color));
    }
    Ok(result)
}
fn packed(data: &[u8], width: usize, height: usize, depth: u8, palette: &[u32],
          mask: Option<&[u8]>)
          -> Result<IconImage, IconError>
{
    if !matches!(depth, 1 | 4 | 8)
    {
        return Err(IconError::Unsupported);
    }
    let stride = (width * usize::from(depth)).div_ceil(8);
    let size = stride.checked_mul(height).ok_or(IconError::TooLarge)?;
    if data.len() < size
    {
        return Err(IconError::Truncated);
    }
    let mask_stride = width.div_ceil(8);
    if mask.is_some_and(|m| m.len() < mask_stride * height)
    {
        return Err(IconError::Truncated);
    }
    let count = width.checked_mul(height).ok_or(IconError::TooLarge)?;
    let mut pixels = allocated(count)?;
    let mut alpha = allocated(count)?;
    for y in 0..height
    {
        for x in 0..width
        {
            let bit = x * usize::from(depth);
            let index = usize::from((data[y * stride + bit / 8]
                                     >> (8 - usize::from(depth) - (bit & 7)))
                                    & ((1u16 << depth) - 1) as u8);
            pixels.push(*palette.get(index).ok_or(IconError::InvalidData)?);
            alpha.push(if mask.is_none_or(|m| m[y * mask_stride + x / 8] & (0x80 >> (x & 7)) != 0)
                       {
                           255
                       }
                       else
                       {
                           0
                       });
        }
    }
    Ok(IconImage { width: width as u16,
                   height: height as u16,
                   depth,
                   pixels,
                   mask: alpha })
}
impl IconFamily
{
    /// Resolve one family through the Resource Manager's current search chain.
    /// Decoded images own their pixels and retain no resource pointers.
    pub fn from_lookup<'a>(id: i16, mut lookup: impl FnMut([u8; 4], i16) -> Option<&'a [u8]>)
                           -> Result<Self, IconError>
    {
        const MEMBERS: [([u8; 4], i16); 11] = [(*b"clut", 4),
                                               (*b"clut", 8),
                                               (*b"ICN#", 0),
                                               (*b"icl4", 0),
                                               (*b"icl8", 0),
                                               (*b"ics#", 0),
                                               (*b"ics4", 0),
                                               (*b"ics8", 0),
                                               (*b"ICON", 0),
                                               (*b"SICN", 0),
                                               (*b"cicn", 0)];
        let empty = ResourceRef { kind:       [0; 4],
                                  id:         0,
                                  name:       None,
                                  attributes: 0,
                                  data:       &[] };
        let mut resources = [empty; MEMBERS.len()];
        let mut count = 0;
        for (kind, fixed) in MEMBERS
        {
            let resource_id = if kind == *b"clut" { fixed } else { id };
            if let Some(data) = lookup(kind, resource_id)
            {
                resources[count] = ResourceRef { kind,
                                                 id: resource_id,
                                                 name: None,
                                                 attributes: 0,
                                                 data };
                count += 1;
            }
        }
        Self::from_resource_refs(&resources[..count], id)
    }

    /// Keyboard icons use their own resource-ID namespace but the small-icon format.
    pub fn keyboard_from_lookup<'a>(id: i16,
                                    mut lookup: impl FnMut([u8; 4], i16) -> Option<&'a [u8]>)
                                    -> Result<Self, IconError>
    {
        Self::from_lookup(id, |kind, resource_id| {
            let kind = match &kind
            {
                b"ics#" => *b"kcs#",
                b"ics4" => *b"kcs4",
                b"ics8" => *b"kcs8",
                b"clut" => kind,
                _ => return None
            };
            lookup(kind, resource_id)
        })
    }

    pub fn keyboard_from_resource_refs(resources: &[ResourceRef<'_>], id: i16)
                                       -> Result<Self, IconError>
    {
        Self::keyboard_from_lookup(id, |kind, id| find(resources, kind, id).map(|r| r.data))
    }

    pub fn from_resources(resources: &[Resource], id: i16) -> Result<Self, IconError>
    {
        Self::from_resource_refs(&refs(resources)?, id)
    }

    pub fn from_resource_refs(resources: &[ResourceRef<'_>], id: i16) -> Result<Self, IconError>
    {
        Self::with_palettes(resources, id, &IconPalettes::from_resource_refs(resources)?)
    }

    pub fn with_palettes(resources: &[ResourceRef<'_>], id: i16, palettes: &IconPalettes)
                         -> Result<Self, IconError>
    {
        let small_count = if let Some(r) = find(resources, *b"SICN", id)
        {
            if r.data.is_empty() || !r.data.len().is_multiple_of(32)
            {
                return Err(IconError::InvalidData);
            }
            r.data.len() / 32
        }
        else
        {
            0
        };
        if !resources.iter().any(|r| {
                                r.id == id
                                && matches!(&r.kind,
                                            b"ICN#"
                                            | b"icl4"
                                            | b"icl8"
                                            | b"ics#"
                                            | b"ics4"
                                            | b"ics8"
                                            | b"ICON"
                                            | b"SICN"
                                            | b"cicn")
                            })
        {
            return Err(IconError::NotFound);
        }
        let mut images = allocated(8usize.checked_add(small_count).ok_or(IconError::TooLarge)?)?;
        for (size, mono, color4, color8) in
            [(32, *b"ICN#", *b"icl4", *b"icl8"), (16, *b"ics#", *b"ics4", *b"ics8")]
        {
            let mono = find(resources, mono, id);
            let bytes = size * size / 8;
            let mask = if let Some(r) = mono
            {
                if r.data.len() != bytes * 2
                {
                    return Err(IconError::InvalidData);
                }
                Some(&r.data[bytes..])
            }
            else
            {
                None
            };
            if let Some(r) = mono
            {
                images.push(packed(&r.data[..bytes], size, size, 1, &[0xffffff, 0], mask)?);
            }
            for (kind, depth, palette) in
                [(color4, 4, &palettes.four[..]), (color8, 8, &palettes.eight[..])]
            {
                if let Some(r) = find(resources, kind, id)
                {
                    if mask.is_none()
                    {
                        return Err(IconError::MissingMask);
                    }
                    images.push(packed(r.data, size, size, depth, palette, mask)?);
                }
            }
        }
        if let Some(r) = find(resources, *b"ICON", id)
        {
            if r.data.len() != 128
            {
                return Err(IconError::InvalidData);
            }
            images.push(packed(r.data, 32, 32, 1, &[0xffffff, 0], None)?);
        }
        if let Some(r) = find(resources, *b"SICN", id)
        {
            if r.data.is_empty() || !r.data.len().is_multiple_of(32)
            {
                return Err(IconError::InvalidData);
            }
            for data in r.data.chunks(32)
            {
                images.push(packed(data, 16, 16, 1, &[0xffffff, 0], None)?);
            }
        }
        if let Some(r) = find(resources, *b"cicn", id)
        {
            images.push(decode_cicn(r.data)?);
        }
        if images.is_empty()
        {
            return Err(IconError::NotFound);
        }
        Ok(Self { id,
                  images })
    }

    pub fn draw(&self, canvas: &mut Canvas, destination: Rect, flags: IconFlags)
                -> Result<(), IconError>
    {
        self.draw_with_display(canvas, destination, flags, IconDisplay::System7Indexed)
    }

    pub fn draw_with_display(&self, canvas: &mut Canvas, destination: Rect, flags: IconFlags,
                             display: IconDisplay)
                             -> Result<(), IconError>
    {
        if destination.is_empty()
        {
            return Ok(());
        }
        let wanted=(i32::from(destination.right)-i32::from(destination.left)).max(i32::from(destination.bottom)-i32::from(destination.top));
        let image = self.images
                        .iter()
                        .min_by_key(|image| {
                            ((i32::from(image.width.max(image.height)) - wanted).abs(),
                             8 - image.depth)
                        })
                        .ok_or(IconError::NotFound)?;
        image.draw_with_display(canvas, destination, flags, display)
    }
}
impl IconImage
{
    pub fn draw(&self, canvas: &mut Canvas, destination: Rect, flags: IconFlags)
                -> Result<(), IconError>
    {
        self.draw_with_display(canvas, destination, flags, IconDisplay::System7Indexed)
    }

    pub fn draw_with_display(&self, canvas: &mut Canvas, destination: Rect, flags: IconFlags,
                             display: IconDisplay)
                             -> Result<(), IconError>
    {
        let size = usize::from(self.width) * usize::from(self.height);
        if self.width == 0
           || self.height == 0
           || self.pixels.len() != size
           || self.mask.len() != size
        {
            return Err(IconError::InvalidData);
        }
        if destination.is_empty()
        {
            return Ok(());
        }
        if flags.open && flags.disabled
        {
            return Err(IconError::Unsupported);
        }
        // Open icons replace their source colors with a patterned mask silhouette.
        // Validate other transforms before mutating any destination pixels.
        let mut inline = None::<[u32; 32 * 32]>;
        let mut large;
        let colors: &[u32] = if flags.open
        {
            &[]
        }
        else if display == IconDisplay::LogicalRgb
                  && !flags.selected
                  && !flags.disabled
                  && self.pixels.iter().all(|pixel| pixel & 0xff000000 == 0)
        {
            &self.pixels
        }
        else
        {
            let target: &mut [u32] = if size <= 32 * 32
            {
                &mut inline.insert([0; 32 * 32])[..size]
            }
            else
            {
                large = allocated(size)?;
                large.resize(size, 0);
                &mut large
            };
            for ((target, &pixel), &mask) in target.iter_mut().zip(&self.pixels).zip(&self.mask)
            {
                *target = if mask == 0 { 0 } else { display.transform(pixel & 0xffffff, flags)? };
            }
            target
        };
        // Original Icon Utilities has a bitmap path for standard target sizes
        // and a region path for larger/scaled targets. Their outline and pattern
        // phases differ; both are retained in original shifted-origin captures.
        let standard_target =
            matches!((i32::from(destination.right) - i32::from(destination.left),
                      i32::from(destination.bottom) - i32::from(destination.top)),
                     (16, 16) | (32, 32));
        let opaque = |x: i32, y: i32| {
            if x < i32::from(destination.left)
               || x >= i32::from(destination.right)
               || y < i32::from(destination.top)
               || y >= i32::from(destination.bottom)
            {
                return false;
            }
            let sx = (i64::from(x) - i64::from(destination.left)) * i64::from(self.width)
                     / (i64::from(destination.right) - i64::from(destination.left));
            let sy = (i64::from(y) - i64::from(destination.top)) * i64::from(self.height)
                     / (i64::from(destination.bottom) - i64::from(destination.top));
            self.mask[sy as usize * usize::from(self.width) + sx as usize] != 0
        };
        let origin = canvas.origin();
        let clip = canvas.clip();
        let left = i32::from(destination.left).max(i32::from(clip.left) - i32::from(origin.h));
        let right = i32::from(destination.right).min(i32::from(clip.right) - i32::from(origin.h));
        let top = i32::from(destination.top).max(i32::from(clip.top) - i32::from(origin.v));
        let bottom =
            i32::from(destination.bottom).min(i32::from(clip.bottom) - i32::from(origin.v));
        for y in top..bottom
        {
            for x in left..right
            {
                let sx = (i64::from(x) - i64::from(destination.left)) * i64::from(self.width)
                         / (i64::from(destination.right) - i64::from(destination.left));
                let sy = (i64::from(y) - i64::from(destination.top)) * i64::from(self.height)
                         / (i64::from(destination.bottom) - i64::from(destination.top));
                let source = sy as usize * usize::from(self.width) + sx as usize;
                if self.mask[source] != 0
                {
                    let target = (y + i32::from(origin.v)) as usize * canvas.width
                                 + (x + i32::from(origin.h)) as usize;
                    canvas.pixels[target] = if flags.open
                    {
                        let inner = if standard_target
                        {
                            [(0, -1), (0, 1), (-1, 0), (1, 0)].iter().all(|&(dx, dy)| {
                                                                         opaque(x + dx, y + dy)
                                                                     })
                        }
                        else
                        {
                            (-1..=1).all(|dy| (-1..=1).all(|dx| opaque(x + dx, y + dy)))
                        };
                        let (px, py) = if standard_target
                        {
                            (x - i32::from(destination.left), y - i32::from(destination.top))
                        }
                        else
                        {
                            (x + i32::from(origin.h), y + i32::from(origin.v))
                        };
                        let stipple = (px + 2 * py).rem_euclid(4) == 0;
                        if !inner || stipple
                        {
                            0
                        }
                        else
                        {
                            display.color(if flags.selected { 0x333366 } else { 0xccccff })
                        }
                    }
                    else
                    {
                        colors[source]
                    };
                }
            }
        }
        Ok(())
    }
}

pub fn decode_cicn(data: &[u8]) -> Result<IconImage, IconError>
{
    if data.len() < 82
    {
        return Err(IconError::Truncated);
    }
    let bounds = rect(data, 6)?;
    let mask_bounds = rect(data, 56)?;
    let mono_bounds = rect(data, 70)?;
    if bounds.is_empty()
    {
        return Err(IconError::InvalidData);
    }
    let width = (i32::from(bounds.right) - i32::from(bounds.left)) as usize;
    let height = (i32::from(bounds.bottom) - i32::from(bounds.top)) as usize;
    if width > 256 || height > 256
    {
        return Err(IconError::TooLarge);
    }
    let depth = word(data, 32)?;
    if !matches!(depth, 1 | 2 | 4 | 8) || word(data, 16)? > 1
    {
        return Err(IconError::Unsupported);
    }
    let stride = usize::from(word(data, 4)? & 0x3fff);
    let mask_stride = usize::from(word(data, 54)? & 0x3fff);
    let mono_stride = usize::from(word(data, 68)? & 0x3fff);
    if mask_bounds != bounds
       || stride < (width * usize::from(depth)).div_ceil(8)
       || mask_stride < width.div_ceil(8)
    {
        return Err(IconError::InvalidData);
    }
    let mask_length = mask_stride * height;
    let mask = data.get(82..82 + mask_length).ok_or(IconError::Truncated)?;
    let mono_height = (i32::from(mono_bounds.bottom) - i32::from(mono_bounds.top)).max(0) as usize;
    let table_offset = 82 + mask_length + mono_stride * mono_height;
    let table = color_table(data.get(table_offset..).ok_or(IconError::Truncated)?)?;
    let table_length = 8 + table.len() * 8;
    let offset = table_offset + table_length;
    let pixels = data.get(offset..offset + stride * height).ok_or(IconError::Truncated)?;
    let mut palette = [None; 256];
    for (index, color) in table
    {
        let slot = palette.get_mut(index).ok_or(IconError::InvalidData)?;
        *slot = Some(color);
    }
    let mut result =
        IconImage { width:  width as u16,
                    height: height as u16,
                    depth:  depth as u8,
                    pixels: allocated(width.checked_mul(height).ok_or(IconError::TooLarge)?)?,
                    mask:   allocated(width.checked_mul(height).ok_or(IconError::TooLarge)?)? };
    for y in 0..height
    {
        for x in 0..width
        {
            let bit = x * usize::from(depth);
            let index = usize::from((pixels[y * stride + bit / 8]
                                     >> (8 - usize::from(depth) - (bit & 7)))
                                    & ((1u16 << depth) - 1) as u8);
            result.pixels.push(palette[index].ok_or(IconError::InvalidData)?);
            result.mask.push(if mask[y * mask_stride + x / 8] & (0x80 >> (x & 7)) != 0
                             {
                                 255
                             }
                             else
                             {
                                 0
                             });
        }
    }
    Ok(result)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleMap
{
    pub kind:    [u8; 4],
    pub entries: Vec<(i16, i16)>
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bundle
{
    pub signature: [u8; 4],
    pub version:   u16,
    pub mappings:  Vec<BundleMap>
}
pub fn decode_bundle(data: &[u8]) -> Result<Bundle, IconError>
{
    let signature = data.get(..4).ok_or(IconError::Truncated)?.try_into().unwrap();
    let version = word(data, 4)?;
    let count = word(data, 6)?.wrapping_add(1);
    let mut cursor = 8;
    if usize::from(count) > data.len().saturating_sub(cursor) / 6
    {
        return Err(IconError::Truncated);
    }
    let mut mappings = allocated(usize::from(count))?;
    for _ in 0..count
    {
        let kind = data.get(cursor..cursor + 4).ok_or(IconError::Truncated)?.try_into().unwrap();
        let count = word(data, cursor + 4)?.wrapping_add(1);
        cursor += 6;
        data.get(cursor..cursor + usize::from(count) * 4).ok_or(IconError::Truncated)?;
        let mut entries = allocated(usize::from(count))?;
        for _ in 0..count
        {
            entries.push((word(data, cursor)? as i16, word(data, cursor + 2)? as i16));
            cursor += 4;
        }
        mappings.push(BundleMap { kind,
                                  entries });
    }
    if cursor != data.len()
    {
        return Err(IconError::InvalidData);
    }
    Ok(Bundle { signature,
                version,
                mappings })
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileReference
{
    pub file_type:     [u8; 4],
    pub local_icon_id: i16,
    pub name:          Vec<u8>
}
pub fn decode_fref(data: &[u8]) -> Result<FileReference, IconError>
{
    let file_type = data.get(..4).ok_or(IconError::Truncated)?.try_into().unwrap();
    let local_icon_id = word(data, 4)? as i16;
    let length = usize::from(*data.get(6).ok_or(IconError::Truncated)?);
    let bytes = data.get(7..7 + length).ok_or(IconError::Truncated)?;
    if data.len() != 7 + length
    {
        return Err(IconError::InvalidData);
    }
    let mut name = allocated(length)?;
    name.extend_from_slice(bytes);
    Ok(FileReference { file_type,
                       local_icon_id,
                       name })
}
pub fn file_icon_id(resources: &[ResourceRef<'_>], creator: [u8; 4], file_type: [u8; 4])
                    -> Result<i16, IconError>
{
    for resource in resources.iter().filter(|r| r.kind == *b"BNDL")
    {
        let bundle = decode_bundle(resource.data)?;
        if bundle.signature != creator
        {
            continue;
        }
        let Some(references) = bundle.mappings.iter().find(|m| m.kind == *b"FREF")
        else
        {
            continue;
        };
        let Some(icons) = bundle.mappings.iter().find(|m| m.kind == *b"ICN#")
        else
        {
            continue;
        };
        for (_, id) in &references.entries
        {
            let reference = find(resources, *b"FREF", *id).ok_or(IconError::NotFound)?;
            let reference = decode_fref(reference.data)?;
            if reference.file_type == file_type
            {
                return icons.entries
                            .iter()
                            .find(|(local, _)| *local == reference.local_icon_id)
                            .map(|(_, id)| *id)
                            .ok_or(IconError::NotFound);
            }
        }
    }
    Err(IconError::NotFound)
}

#[cfg(test)]
mod tests
{
    use super::*;

    fn resource(kind: [u8; 4], id: i16, data: Vec<u8>) -> Resource
    {
        Resource { kind,
                   id,
                   name: None,
                   attributes: 0,
                   data }
    }

    #[test]
    fn family_lookup_requires_masks_and_preserves_transparency()
    {
        let mut bytes = vec![0; 256];
        bytes[0] = 0x80;
        bytes[128] = 0xc0;
        let resources =
            vec![resource(*b"ICN#", 42, bytes), resource(*b"icl8", 42, vec![255; 1024])];
        let family =
            IconFamily::from_lookup(42, |kind, id| {
                resources.iter().find(|r| r.kind == kind && r.id == id).map(|r| r.data.as_slice())
            }).unwrap();
        assert_eq!(family.images.len(), 2);
        let mut pixels = vec![0x123456; 64];
        let mut canvas = Canvas::new(&mut pixels, 8, 8);
        canvas.set_origin(2, 3);
        canvas.set_clip(Rect::new(1, 0, 3, 1));
        family.draw(&mut canvas, Rect::new(0, 0, 32, 32), IconFlags::default()).unwrap();
        // The first source pixel is clipped; the second is opaque black; the third is transparent.
        assert_eq!(pixels[3 * 8 + 2], 0x123456);
        assert_eq!(pixels[3 * 8 + 3], 0);
        assert_eq!(pixels[3 * 8 + 4], 0x123456);
        let no_mask = [resource(*b"icl8", 1, vec![0; 1024])];
        assert_eq!(IconFamily::from_resources(&no_mask, 1).unwrap_err(), IconError::MissingMask);
        assert_eq!(IconFamily::from_resources(&resources, 99).unwrap_err(), IconError::NotFound);
    }

    #[test]
    fn keyboard_families_keep_their_separate_resource_namespace()
    {
        let mut ordinary = vec![0; 64];
        ordinary[32..].fill(255);
        let keyboard = vec![255; 64];
        let resources = vec![resource(*b"ics#", 7, ordinary),
                             resource(*b"kcs#", 7, keyboard),
                             resource(*b"kcs4", 7, vec![0; 128])];
        let refs = refs(&resources).unwrap();
        let normal = IconFamily::from_resource_refs(&refs, 7).unwrap();
        let keyboard = IconFamily::keyboard_from_resource_refs(&refs, 7).unwrap();
        assert_eq!(normal.images.len(), 1);
        assert_eq!(normal.images[0].pixels[0], 0xffffff);
        assert_eq!(keyboard.images.len(), 2);
        assert_eq!(keyboard.images[0].pixels[0], 0);
        assert_eq!(keyboard.images[1].pixels[0], 0xffffff);
    }

    #[test]
    fn standalone_lists_and_sparse_color_icon_are_checked()
    {
        let mut list = vec![0; 64];
        list[32] = 0x80;
        let family = IconFamily::from_resources(&[resource(*b"SICN", 9, list)], 9).unwrap();
        assert_eq!(family.images.len(), 2);
        assert_eq!(family.images[1].pixels[0], 0);
        // A 1x1 CIcon with no mono bitmap and a sparse palette containing index1 only.
        let mut data = vec![0; 82];
        data[4..6].copy_from_slice(&0x8001u16.to_be_bytes());
        data[6..14].copy_from_slice(&Rect::new(0, 0, 1, 1).to_be_bytes());
        data[32..34].copy_from_slice(&1u16.to_be_bytes());
        data[54..56].copy_from_slice(&1u16.to_be_bytes());
        data[56..64].copy_from_slice(&Rect::new(0, 0, 1, 1).to_be_bytes());
        data.push(0x80);
        data.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        data.push(0x80);
        let image = decode_cicn(&data).unwrap();
        assert_eq!(image.pixels, [0]);
        assert_eq!(image.mask, [255]);
        for length in 0..data.len()
        {
            assert!(decode_cicn(&data[..length]).is_err());
        }
        data[4..6].copy_from_slice(&0x8000u16.to_be_bytes());
        assert_eq!(decode_cicn(&data).unwrap_err(), IconError::InvalidData);
    }

    #[test]
    fn bundles_resolve_local_icon_ids_through_fref()
    {
        let mut bundle = b"TEST\0\0\0\x01ICN#\0\0\0\x07\x12\x34FREF\0\0\0\x02\0\x63".to_vec();
        let resources = vec![resource(*b"BNDL", 128, bundle.clone()),
                             resource(*b"FREF", 99, b"TEXT\0\x07\0".to_vec())];
        assert_eq!(file_icon_id(&refs(&resources).unwrap(), *b"TEST", *b"TEXT"), Ok(0x1234));
        assert_eq!(file_icon_id(&refs(&resources).unwrap(), *b"NOPE", *b"TEXT"),
                   Err(IconError::NotFound));
        bundle.pop();
        assert!(decode_bundle(&bundle).is_err());
        assert!(decode_fref(b"TEXT\0\0\x03hi").is_err());
        assert_eq!(IconFlags::from_transform(2), Err(IconError::Unsupported));
    }

    #[test]
    fn open_icons_replace_source_colors_and_keep_bitmap_pattern_phase()
    {
        let flags = IconFlags::from_transform(3).unwrap();
        assert!(flags.open && !flags.disabled && !flags.selected);
        assert!(IconFlags::from_transform(0x4003).unwrap().selected);
        let mut image = IconImage { width:  16,
                                    height: 16,
                                    depth:  8,
                                    pixels: vec![0x123456; 256],
                                    mask:   vec![255; 256] };
        image.mask[4 * 16 + 4] = 0;
        let mut pixels = vec![0xffffff; 24 * 24];
        let mut canvas = Canvas::new(&mut pixels, 24, 24);
        canvas.set_origin(1, 1);
        image.draw(&mut canvas, Rect::new(2, 3, 18, 19), flags).unwrap();
        assert_eq!(pixels[(4 + 4) * 24 + 3 + 4], 0xffffff);
        // A missing diagonal neighbor is not part of the standard bitmap outline.
        assert_eq!(pixels[(4 + 5) * 24 + 3 + 5], 0xdadaff);
        assert_eq!(pixels[4 * 24 + 3], 0);
        // Bitmap stipple phase is local to the icon, independent of Canvas origin.
        assert_eq!(pixels[(4 + 1) * 24 + 3 + 2], 0);
        assert_eq!(pixels[(4 + 1) * 24 + 3 + 3], 0xdadaff);
    }

    #[test]
    fn original_icon_transforms_and_unsupported_colors_are_atomic()
    {
        let selected = IconFlags { selected: true,
                                   disabled: false,
                                   open:     false };
        let disabled = IconFlags { selected: false,
                                   disabled: true,
                                   open:     false };
        let both = IconFlags { selected: true,
                               disabled: true,
                               open:     false };
        assert_eq!(IconDisplay::System7Indexed.transform(0xffffff, selected), Ok(0x969696));
        assert_eq!(IconDisplay::System7Indexed.transform(0, disabled), Ok(0x969696));
        assert_eq!(IconDisplay::System7Indexed.transform(0xffffff, both), Ok(0xcdcdcd));
        // The original table leaves this red ramp entry unchanged when selected.
        assert_eq!(IconDisplay::System7Indexed.transform(0xee0000, selected), Ok(0xf30000));
        let image = IconImage { width:  2,
                                height: 1,
                                depth:  8,
                                pixels: vec![0, 0x123456],
                                mask:   vec![255, 255] };
        let mut pixels = [0xabcdef; 2];
        let mut canvas = Canvas::new(&mut pixels, 2, 1);
        assert_eq!(image.draw(&mut canvas, Rect::new(0, 0, 2, 1), selected),
                   Err(IconError::Unsupported));
        assert_eq!(pixels, [0xabcdef; 2]);
    }

    #[test]
    fn display_palette_is_explicit_and_logical_colors_are_unchanged()
    {
        assert_eq!(IconPalettes::default().four[12], 0xc0c0c0);
        assert_eq!(IconDisplay::LogicalRgb.color(0xcccccc), 0xcccccc);
        assert_eq!(IconDisplay::System7Indexed.color(0xcccccc), 0xdadada);
        assert_eq!(IconDisplay::System7Indexed.color(0x123456), 0x123456);
    }
}
