//! Checked bitmap transfer into the resident RGB framebuffer.
#![forbid(unsafe_code)]
#[cfg(test)]
use alloc::vec;

use super::{Canvas, Point, Rect, region::Region};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferMode(u8);
impl TransferMode
{
    pub const PAT_COPY: Self = Self(8);
    pub const SRC_BIC: Self = Self(3);
    pub const SRC_COPY: Self = Self(0);
    pub const SRC_OR: Self = Self(1);
    pub const SRC_XOR: Self = Self(2);

    pub fn new(mode: i16) -> Option<Self>
    {
        (0..16).contains(&mode).then_some(Self(mode as u8))
    }

    pub fn number(self) -> u8
    {
        self.0
    }

    /// One-bit patterns use the port's colors, independently of RGB bit encoding.
    pub fn pattern(self, mut bit: bool, destination: u32, foreground: u32, background: u32) -> u32
    {
        if self.0 & 4 != 0
        {
            bit = !bit;
        }
        (match self.0 & 3
        {
            0 =>
            {
                if bit
                {
                    foreground
                }
                else
                {
                    background
                }
            }
            1 =>
            {
                if bit
                {
                    foreground
                }
                else
                {
                    destination
                }
            }
            2 =>
            {
                if bit
                {
                    !destination
                }
                else
                {
                    destination
                }
            }
            _ =>
            {
                if bit
                {
                    background
                }
                else
                {
                    destination
                }
            }
        })
        & 0xffffff
    }

    /// Color QuickDraw's Boolean source modes interpolate components, rather than
    /// treating RGB zero as a set black bitmap bit. Colored XOR is documented as
    /// unchanged; only black/white source pixels invert or preserve destination.
    pub fn source(self, mut source: u32, destination: u32, foreground: u32, background: u32)
                  -> u32
    {
        source &= 0xffffff;
        if self.0 & 4 != 0
        {
            source ^= 0xffffff;
        }
        if self.0 & 3 == 2
        {
            return if source == 0 { (!destination) & 0xffffff } else { destination & 0xffffff };
        }
        let mut result = 0;
        for shift in [0, 8, 16]
        {
            let intensity = (source >> shift) & 255;
            let d = (destination >> shift) & 255;
            let f = (foreground >> shift) & 255;
            let b = (background >> shift) & 255;
            let value = match self.0 & 3
            {
                0 => (f * (255 - intensity) + b * intensity) / 255,
                1 => (f * (255 - intensity) + d * intensity) / 255,
                _ => (b * (255 - intensity) + d * intensity) / 255
            };
            result |= value << shift;
        }
        result
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RasterError
{
    InvalidBounds,
    InvalidStride,
    InvalidPalette,
    InvalidDepth,
    Truncated,
    TooLarge,
    Allocation
}
impl From<super::region::RegionError> for RasterError
{
    fn from(error: super::region::RegionError) -> Self
    {
        use super::region::RegionError as R;
        match error
        {
            R::Allocation => Self::Allocation,
            R::TooLarge => Self::TooLarge,
            R::Truncated
            | R::InvalidSize
            | R::InvalidBounds
            | R::InvalidData
            | R::CoordinateOverflow => Self::InvalidBounds
        }
    }
}
impl From<super::port::GraphicsError> for RasterError
{
    fn from(error: super::port::GraphicsError) -> Self
    {
        use super::port::GraphicsError as G;
        match error
        {
            G::Allocation => Self::Allocation,
            G::TooLarge => Self::TooLarge,
            G::InvalidSize | G::CoordinateOverflow => Self::InvalidBounds
        }
    }
}
#[derive(Clone, Copy)]
pub enum Bitmap<'a>
{
    Mono
    {
        data: &'a [u8], row_bytes: usize, bounds: Rect
    },
    Indexed
    {
        data:      &'a [u8],
        row_bytes: usize,
        bounds:    Rect,
        depth:     u8,
        palette:   &'a [u32]
    },
    Rgb16
    {
        data: &'a [u8], row_bytes: usize, bounds: Rect
    },
    Rgb24
    {
        data: &'a [u8], row_bytes: usize, bounds: Rect
    },
    Xrgb32
    {
        data: &'a [u8], row_bytes: usize, bounds: Rect
    },
    Rgb32
    {
        pixels: &'a [u32], row_pixels: usize, bounds: Rect
    }
}
impl Bitmap<'_>
{
    pub fn bounds(&self) -> Rect
    {
        match self
        {
            Self::Mono { bounds,
                         .. }
            | Self::Indexed { bounds,
                            .. }
            | Self::Rgb16 { bounds,
                          .. }
            | Self::Rgb24 { bounds,
                          .. }
            | Self::Xrgb32 { bounds,
                           .. }
            | Self::Rgb32 { bounds,
                          .. } => *bounds
        }
    }

    pub fn validate(&self) -> Result<(), RasterError>
    {
        let b = self.bounds();
        if b.is_empty()
        {
            return Err(RasterError::InvalidBounds);
        }
        let w = (i32::from(b.right) - i32::from(b.left)) as usize;
        let h = (i32::from(b.bottom) - i32::from(b.top)) as usize;
        let (stride, needed, len) = match self
        {
            Self::Mono { data,
                         row_bytes,
                         .. } => (*row_bytes, w.div_ceil(8), data.len()),
            Self::Indexed { data,
                            row_bytes,
                            depth,
                            palette,
                            .. } =>
            {
                if !matches!(depth, 1 | 2 | 4 | 8)
                {
                    return Err(RasterError::InvalidDepth);
                }
                if palette.len() < (1usize << depth)
                {
                    return Err(RasterError::InvalidPalette);
                }
                (*row_bytes, (w * usize::from(*depth)).div_ceil(8), data.len())
            }
            Self::Rgb16 { data,
                          row_bytes,
                          .. } => (*row_bytes, w * 2, data.len()),
            Self::Rgb24 { data,
                          row_bytes,
                          .. } => (*row_bytes, w * 3, data.len()),
            Self::Xrgb32 { data,
                           row_bytes,
                           .. } => (*row_bytes, w * 4, data.len()),
            Self::Rgb32 { pixels,
                          row_pixels,
                          .. } => (*row_pixels, w, pixels.len())
        };
        if stride < needed
        {
            return Err(RasterError::InvalidStride);
        }
        let length = (h - 1).checked_mul(stride)
                            .and_then(|n| n.checked_add(needed))
                            .ok_or(RasterError::TooLarge)?;
        if length > len
        {
            return Err(RasterError::Truncated);
        }
        Ok(())
    }

    pub(super) fn pixel(&self, p: Point) -> u32
    {
        let b = self.bounds();
        let x = (i32::from(p.h) - i32::from(b.left)) as usize;
        let y = (i32::from(p.v) - i32::from(b.top)) as usize;
        match self
        {
            Self::Mono { data,
                         row_bytes,
                         .. } =>
            {
                if data[y * row_bytes + x / 8] & (0x80 >> (x & 7)) != 0 { 0 } else { 0xffffff }
            }
            Self::Indexed { data,
                            row_bytes,
                            depth,
                            palette,
                            .. } =>
            {
                let bits = x * usize::from(*depth);
                let shift = 8 - usize::from(*depth) - (bits & 7);
                palette[((data[y * row_bytes + bits / 8] >> shift) & ((1u16 << depth) - 1) as u8)
                        as usize]
                & 0xffffff
            }
            Self::Rgb16 { data,
                          row_bytes,
                          .. } =>
            {
                let offset = y * row_bytes + x * 2;
                let value = u16::from_be_bytes([data[offset], data[offset + 1]]);
                let expand = |n: u16| u32::from((n << 3) | (n >> 2));
                (expand((value >> 10) & 31) << 16)
                | (expand((value >> 5) & 31) << 8)
                | expand(value & 31)
            }
            Self::Rgb24 { data,
                          row_bytes,
                          .. } =>
            {
                let offset = y * row_bytes + x * 3;
                u32::from_be_bytes([0, data[offset], data[offset + 1], data[offset + 2]])
            }
            Self::Xrgb32 { data,
                           row_bytes,
                           .. } =>
            {
                let offset = y * row_bytes + x * 4;
                u32::from_be_bytes([0, data[offset + 1], data[offset + 2], data[offset + 3]])
            }
            Self::Rgb32 { pixels,
                          row_pixels,
                          .. } => pixels[y * row_pixels + x] & 0xffffff
        }
    }
}
#[allow(clippy::too_many_arguments)]
pub fn copy_bits(canvas: &mut Canvas, source: &Bitmap<'_>, source_rect: Rect,
                 destination_rect: Rect, mode: TransferMode, mask: Option<&Region>,
                 foreground: u32, background: u32)
                 -> Result<(), RasterError>
{
    source.validate()?;
    let bounds = source.bounds();
    if source_rect.is_empty() || destination_rect.is_empty()
    {
        return Ok(());
    }
    if source_rect.left < bounds.left
       || source_rect.top < bounds.top
       || source_rect.right > bounds.right
       || source_rect.bottom > bounds.bottom
    {
        return Err(RasterError::InvalidBounds);
    }
    let o = canvas.origin();
    let clip = canvas.clip();
    let left = i32::from(destination_rect.left).max(i32::from(clip.left) - i32::from(o.h));
    let right = i32::from(destination_rect.right).min(i32::from(clip.right) - i32::from(o.h));
    let top = i32::from(destination_rect.top).max(i32::from(clip.top) - i32::from(o.v));
    let bottom = i32::from(destination_rect.bottom).min(i32::from(clip.bottom) - i32::from(o.v));
    let sw = i64::from(source_rect.right) - i64::from(source_rect.left);
    let sh = i64::from(source_rect.bottom) - i64::from(source_rect.top);
    let dw = i64::from(destination_rect.right) - i64::from(destination_rect.left);
    let dh = i64::from(destination_rect.bottom) - i64::from(destination_rect.top);
    for y in top..bottom
    {
        for x in left..right
        {
            let point = Point { h: x as i16,
                                v: y as i16 };
            if mask.is_some_and(|mask| !mask.contains(point))
            {
                continue;
            }
            let sx = i64::from(source_rect.left)
                     + (i64::from(x) - i64::from(destination_rect.left)) * sw / dw;
            let sy = i64::from(source_rect.top)
                     + (i64::from(y) - i64::from(destination_rect.top)) * sh / dh;
            let pixel = source.pixel(Point { h: sx as i16,
                                             v: sy as i16 });
            let offset =
                (y + i32::from(o.v)) as usize * canvas.width + (x + i32::from(o.h)) as usize;
            canvas.pixels[offset] =
                mode.source(pixel, canvas.pixels[offset], foreground, background);
        }
    }
    Ok(())
}
/// Snapshot first so overlapping self-copies have memmove-style source semantics.
#[allow(clippy::too_many_arguments)]
pub fn copy_within(canvas: &mut Canvas, source_rect: Rect, destination_rect: Rect,
                   mode: TransferMode, mask: Option<&Region>, foreground: u32, background: u32)
                   -> Result<(), RasterError>
{
    if source_rect.is_empty()
    {
        return Ok(());
    }
    let o = canvas.origin();
    let width = i32::from(source_rect.right) - i32::from(source_rect.left);
    let height = i32::from(source_rect.bottom) - i32::from(source_rect.top);
    if i64::from(width) * i64::from(height) > 16_777_216
    {
        return Err(RasterError::TooLarge);
    }
    let mut pixels =
        super::storage::vector(width as usize * height as usize).map_err(|_| {
                                                                    RasterError::Allocation
                                                                })?;
    for y in source_rect.top..source_rect.bottom
    {
        for x in source_rect.left..source_rect.right
        {
            let xx = i32::from(x) + i32::from(o.h);
            let yy = i32::from(y) + i32::from(o.v);
            if xx < 0 || yy < 0 || xx >= canvas.width as i32 || yy >= canvas.height as i32
            {
                return Err(RasterError::InvalidBounds);
            }
            pixels.push(canvas.pixels[yy as usize * canvas.width + xx as usize]);
        }
    }
    copy_bits(canvas,
              &Bitmap::Rgb32 { pixels:     &pixels,
                               row_pixels: width as usize,
                               bounds:     source_rect },
              source_rect,
              destination_rect,
              mode,
              mask,
              foreground,
              background)
}

#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn all_boolean_modes_have_documented_black_white_behavior()
    {
        for mode in 0..16
        {
            let mode = TransferMode::new(mode).unwrap();
            for bit in [false, true]
            {
                for dest in [0, 0xffffff]
                {
                    assert_eq!(mode.pattern(bit, dest, 0, 0xffffff),
                               mode.source(if bit { 0 } else { 0xffffff }, dest, 0, 0xffffff));
                }
            }
        }
        assert!(TransferMode::new(16).is_none());
        assert_eq!(TransferMode::SRC_OR.source(0x808080, 0x808080, 0, 0xffffff), 0x404040);
    }
    #[test]
    fn scaled_mono_mask_stride_and_overlapping_rgb_copy()
    {
        let data = [0x80];
        let bitmap = Bitmap::Mono { data:      &data,
                                    row_bytes: 1,
                                    bounds:    Rect::new(0, 0, 2, 1) };
        let mut pixels = [0x123456; 16];
        let mut canvas = Canvas::new(&mut pixels, 8, 2);
        let mask = Region::from_rect(Rect::new(1, 0, 7, 2));
        copy_bits(&mut canvas,
                  &bitmap,
                  Rect::new(0, 0, 2, 1),
                  Rect::new(0, 0, 8, 2),
                  TransferMode::SRC_COPY,
                  Some(&mask),
                  0,
                  0xffffff).unwrap();
        assert_eq!(canvas.pixels,
                   [0x123456, 0, 0, 0, 0xffffff, 0xffffff, 0xffffff, 0x123456, 0x123456, 0, 0, 0,
                    0xffffff, 0xffffff, 0xffffff, 0x123456]);
        canvas.pixels[..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        copy_within(&mut canvas,
                    Rect::new(0, 0, 6, 1),
                    Rect::new(2, 0, 8, 1),
                    TransferMode::SRC_COPY,
                    None,
                    0,
                    0xffffff).unwrap();
        assert_eq!(&canvas.pixels[..8], &[1, 2, 1, 2, 3, 4, 5, 6]);
        assert_eq!(Bitmap::Mono { data:      &[],
                                  row_bytes: 1,
                                  bounds:    Rect::new(0, 0, 8, 1) }.validate(),
                   Err(RasterError::Truncated));
    }
    #[test]
    fn all_indexed_depths_and_direct_byte_orders_are_checked()
    {
        let bounds = Rect::new(-2, 3, 0, 4);
        let palette = [0x112233, 0x445566, 0x778899, 0xaabbcc];
        for (depth, byte) in [(1, 0x40), (2, 0x10), (4, 0x01), (8, 0x00)]
        {
            let mut all = [0u32; 256];
            all[..4].copy_from_slice(&palette);
            let data = if depth == 8 { vec![0, 1] } else { vec![byte] };
            let bitmap = Bitmap::Indexed { data: &data,
                                           row_bytes: data.len(),
                                           bounds,
                                           depth,
                                           palette: &all };
            bitmap.validate().unwrap();
            assert_eq!(bitmap.pixel(Point { h: -2,
                                            v: 3 }),
                       palette[0]);
            assert_eq!(bitmap.pixel(Point { h: -1,
                                            v: 3 }),
                       palette[1]);
        }
        let rgb16 = [0x7c, 0x00, 0x03, 0xe0];
        let rgb24 = [255, 0, 0, 0, 255, 0];
        let xrgb = [99, 255, 0, 0, 88, 0, 255, 0];
        for bitmap in [Bitmap::Rgb16 { data: &rgb16,
                                       row_bytes: 4,
                                       bounds },
                       Bitmap::Rgb24 { data: &rgb24,
                                       row_bytes: 6,
                                       bounds },
                       Bitmap::Xrgb32 { data: &xrgb,
                                        row_bytes: 8,
                                        bounds }]
        {
            bitmap.validate().unwrap();
            assert_eq!(bitmap.pixel(Point { h: -2,
                                            v: 3 }),
                       0xff0000);
            assert_eq!(bitmap.pixel(Point { h: -1,
                                            v: 3 }),
                       0x00ff00);
        }
        assert_eq!(Bitmap::Indexed { data: &[0],
                                     row_bytes: 1,
                                     bounds,
                                     depth: 4,
                                     palette: &palette }.validate(),
                   Err(RasterError::InvalidPalette));
    }
}
