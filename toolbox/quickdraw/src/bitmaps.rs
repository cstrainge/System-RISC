//! Borrowed pixel buffers are copied by resident System for retained window drawing.
use mac_traps::{BitmapCommand, BitmapData};

use crate::{Rect, Result,
            graphics::{GraphicsPort, TransferMode}};
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
    Rgb32
    {
        pixels: &'a [u32], row_pixels: usize, bounds: Rect
    }
}
impl Bitmap<'_>
{
    fn command<'a>(&'a self, source: Rect, destination: Rect, mode: TransferMode, mask: &'a [u8])
                   -> BitmapCommand<'a>
    {
        let (format, row_bytes, bounds, data, palette) = match self
        {
            Self::Mono { data,
                         row_bytes,
                         bounds } => (1, *row_bytes, *bounds, BitmapData::Bytes(data), &[][..]),
            Self::Indexed { data,
                            row_bytes,
                            bounds,
                            depth,
                            palette } =>
            {
                (*depth as u32, *row_bytes, *bounds, BitmapData::Bytes(data), *palette)
            }
            Self::Rgb16 { data,
                          row_bytes,
                          bounds } => (16, *row_bytes, *bounds, BitmapData::Bytes(data), &[][..]),
            Self::Rgb24 { data,
                          row_bytes,
                          bounds } => (24, *row_bytes, *bounds, BitmapData::Bytes(data), &[][..]),
            Self::Rgb32 { pixels,
                          row_pixels,
                          bounds } =>
            {
                (32, row_pixels.saturating_mul(4), *bounds, BitmapData::Pixels(pixels), &[][..])
            }
        };
        BitmapCommand { format,
                        row_bytes,
                        bounds,
                        data,
                        palette,
                        source_rect: source,
                        destination_rect: destination,
                        mask,
                        mode: mode as i16,
                        foreground: 0,
                        background: 0xffffff }
    }

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
            | Self::Rgb32 { bounds,
                          .. } => *bounds
        }
    }
}
impl GraphicsPort<'_>
{
    pub fn copy_bits(self, bitmap: &Bitmap<'_>, source: Rect, destination: Rect,
                     mode: TransferMode, mask: &[u8])
                     -> Result<()>
    {
        self.client.copy_bits(self.target, bitmap.command(source, destination, mode, mask))
    }
}
impl crate::SurfacePort<'_>
{
    pub fn copy_bits(&mut self, bitmap: &Bitmap<'_>, source: Rect, destination: Rect,
                     mode: TransferMode, mask: &[u8])
                     -> Result<()>
    {
        self.connection.copy_bits(bitmap.command(source, destination, mode, mask))
    }
}
