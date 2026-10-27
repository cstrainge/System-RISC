//! Owned bitmap snapshots preserve caller lifetimes across retained window redraws.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{quickdraw::BitmapCall, *};

use super::{Canvas,
            raster::{self, Bitmap, TransferMode},
            region::{BooleanOp, Region},
            service::Port};
pub struct BitmapDrawing
{
    data:        Vec<u8>,
    pixels:      Vec<u32>,
    palette:     Vec<u32>,
    format:      u32,
    row_bytes:   usize,
    bounds:      Rect,
    source:      Rect,
    destination: Rect,
    mode:        TransferMode,
    mask:        Option<Region>,
    foreground:  u32,
    background:  u32,
    pub port:    Port
}
impl BitmapDrawing
{
    pub fn new(call: &BitmapCall, data: &[u8], palette: &[u32], mask: &[u8], port: Port)
               -> Result<Self, Status>
    {
        if !matches!(call.format, 1 | 2 | 4 | 8 | 16 | 24 | 32)
        {
            return Err(NOT_SUPPORTED);
        }
        let mode = TransferMode::new(call.mode).ok_or(NOT_SUPPORTED)?;
        let pixels = if call.format == 32
        {
            if !data.len().is_multiple_of(4) || !call.row_bytes.is_multiple_of(4)
            {
                return Err(BAD_ARGUMENT);
            }
            let mut pixels = super::storage::vector(data.len() / 4).map_err(|_| NO_MEMORY)?;
            pixels.extend(data.as_chunks::<4>().0.iter().map(|b| u32::from_ne_bytes(*b)));
            pixels
        }
        else
        {
            Vec::new()
        };
        let drawing = Self { data: if call.format == 32
                             {
                                 Vec::new()
                             }
                             else
                             {
                                 super::storage::copy(data).map_err(|_| NO_MEMORY)?
                             },
                             pixels,
                             palette: super::storage::copy(palette).map_err(|_| NO_MEMORY)?,
                             format: call.format,
                             row_bytes: call.row_bytes,
                             bounds: call.bounds,
                             source: call.source_rect,
                             destination: call.destination_rect,
                             mode,
                             mask: if mask.is_empty()
                             {
                                 None
                             }
                             else
                             {
                                 Some(Region::from_be_bytes(mask).map_err(|_| BAD_ARGUMENT)?)
                             },
                             foreground: call.foreground,
                             background: call.background,
                             port };
        drawing.bitmap()?.validate().map_err(|_| BAD_ARGUMENT)?;
        Ok(drawing)
    }

    fn bitmap(&self) -> Result<Bitmap<'_>, Status>
    {
        Ok(match self.format
        {
            1 if self.palette.is_empty() =>
            {
                Bitmap::Mono { data:      &self.data,
                               row_bytes: self.row_bytes,
                               bounds:    self.bounds }
            }
            1 | 2 | 4 | 8 =>
            {
                Bitmap::Indexed { data:      &self.data,
                                  row_bytes: self.row_bytes,
                                  bounds:    self.bounds,
                                  depth:     self.format as u8,
                                  palette:   &self.palette }
            }
            16 =>
            {
                Bitmap::Rgb16 { data:      &self.data,
                                row_bytes: self.row_bytes,
                                bounds:    self.bounds }
            }
            24 =>
            {
                Bitmap::Rgb24 { data:      &self.data,
                                row_bytes: self.row_bytes,
                                bounds:    self.bounds }
            }
            32 =>
            {
                Bitmap::Rgb32 { pixels:     &self.pixels,
                                row_pixels: self.row_bytes / 4,
                                bounds:     self.bounds }
            }
            _ => return Err(NOT_SUPPORTED)
        })
    }

    pub fn storage(&self) -> usize
    {
        self.data.len() + self.pixels.len() * 4 + self.palette.len() * 4
    }

    pub fn paint(&self, canvas: &mut Canvas<'_>) -> Result<(), Status>
    {
        let origin = canvas.origin();
        canvas.set_origin(origin.h.wrapping_add(self.port.origin.h),
                          origin.v.wrapping_add(self.port.origin.v));
        let combined = match (&self.mask, &self.port.state.clip)
        {
            (Some(a), Some(b)) => Some(a.combine(b, BooleanOp::Intersection)),
            (a, None) => a.clone(),
            (None, b) => b.clone()
        };
        let result = raster::copy_bits(canvas,
                                       &self.bitmap()?,
                                       self.source,
                                       self.destination,
                                       self.mode,
                                       combined.as_ref(),
                                       self.foreground,
                                       self.background).map_err(|_| BAD_ARGUMENT);
        canvas.set_origin(origin.h, origin.v);
        result
    }
}
