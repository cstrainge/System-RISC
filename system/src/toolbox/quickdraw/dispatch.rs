//! Surface-service ABI adapter. Rasterization and fonts below this layer are safe Rust.
use mac_abi::{traps::*, *};

use super::{SurfaceExt,
            font::{CHICAGO_12, GENEVA_9}};

/// # Safety
/// The request and any surface/text pointers must borrow valid, exclusive memory
/// for their declared lengths. Native modules currently share an M-mode address space.
pub unsafe extern "C" fn surface_gate(number: u32, request: *mut (), size: usize) -> i64
{
    if number != SURFACE
    {
        return NOT_SUPPORTED as i64;
    }
    if request.is_null()
       || !(request as usize).is_multiple_of(core::mem::align_of::<SurfaceCall>())
       || size != core::mem::size_of::<SurfaceCall>()
    {
        return BAD_ARGUMENT as i64;
    }
    let call = unsafe { &mut *request.cast::<SurfaceCall>() };
    let Some(surface) = (unsafe { call.surface.as_mut() })
    else
    {
        return BAD_ARGUMENT as i64;
    };
    let Some(mut canvas) = (unsafe { surface.canvas() })
    else
    {
        return BAD_ARGUMENT as i64;
    };
    let font = match call.font
    {
        0 => CHICAGO_12,
        1 => GENEVA_9,
        _ => return NOT_SUPPORTED as i64
    };
    let text = if call.text_length == 0
    {
        ""
    }
    else
    {
        if call.text.is_null() || call.text_length > 65535
        {
            return BAD_ARGUMENT as i64;
        }
        let bytes = unsafe { core::slice::from_raw_parts(call.text, call.text_length) };
        match core::str::from_utf8(bytes)
        {
            Ok(s) => s,
            Err(_) => return BAD_ARGUMENT as i64
        }
    };
    match call.operation
    {
        0 => canvas.paint_rect(call.bounds, call.foreground),
        1 => canvas.frame_rect(call.bounds, call.foreground),
        2 => canvas.fill_rect(call.bounds, call.pattern, call.foreground, call.background),
        3 => canvas.invert_rect(call.bounds),
        4 => call.width = font.width(text),
        5 =>
        {
            let metrics = font.metrics();
            call.ascent = metrics.ascent;
            call.descent = metrics.descent;
            call.leading = metrics.leading;
        }
        6 => font.draw_clipped(&mut canvas, call.x, call.y, text, call.limit, call.foreground),
        _ => return NOT_SUPPORTED as i64
    }
    OK as i64
}
