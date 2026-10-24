//! Checked image transport; decoding and compositing are resident services.
use mac_abi::{images::*, *};

use crate::{Client, SurfaceConnection};
fn reply(status: i64, call: &ImageCall) -> images::Result<ImageInfo>
{
    if status == 0
    {
        Ok(call.info)
    }
    else
    {
        Err(ImageError { status: status as Status,
                         kind:   if call.error_kind == 0 { SERVICE_ERROR } else { call.error_kind } })
    }
}
impl Client<'_>
{
    pub fn png_info(self, data: &[u8]) -> images::Result<ImageInfo>
    {
        let mut call = ImageCall { data: data.as_ptr(),
                                   length: data.len(),
                                   ..Default::default() };
        reply(self.invoke(traps::IMAGE, &mut call), &call)
    }

    pub fn png_draw(self, target: quickdraw::DrawingTarget, data: &[u8], destination: Rect)
                    -> images::Result<()>
    {
        let mut call = ImageCall { operation: DRAW,
                                   target: target.into(),
                                   data: data.as_ptr(),
                                   length: data.len(),
                                   destination,
                                   ..Default::default() };
        reply(self.invoke(traps::IMAGE, &mut call), &call).map(|_| ())
    }

    pub fn png_pixels(self, data: &[u8], pixels: &mut [u32], width: usize, height: usize,
                      destination: Rect, clip: Rect)
                      -> images::Result<()>
    {
        if width == 0
           || height == 0
           || width > i16::MAX as usize
           || height > i16::MAX as usize
           || width.checked_mul(height) != Some(pixels.len())
           || pixels.len() > 640 * 480
        {
            return Err(ImageError { status: BAD_ARGUMENT,
                                    kind:   INVALID_DESTINATION });
        }
        let mut surface = Surface { pixels: pixels.as_mut_ptr(),
                                    width: width as u32,
                                    height: height as u32,
                                    origin: Point::default(),
                                    clip };
        let mut call = ImageCall { operation: DRAW,
                                   surface: &mut surface,
                                   data: data.as_ptr(),
                                   length: data.len(),
                                   destination,
                                   ..Default::default() };
        reply(self.invoke(traps::IMAGE, &mut call), &call).map(|_| ())
    }
}
impl SurfaceConnection<'_>
{
    pub fn png(&mut self, data: &[u8], destination: Rect) -> images::Result<()>
    {
        let mut call = ImageCall { operation: DRAW,
                                   surface: self.surface,
                                   data: data.as_ptr(),
                                   length: data.len(),
                                   destination,
                                   ..Default::default() };
        let status = unsafe {
            (self.gate)(traps::IMAGE,
                        (&mut call as *mut ImageCall).cast(),
                        core::mem::size_of::<ImageCall>())
        };
        reply(status, &call).map(|_| ())
    }
}
