//! Control definitions. Full Control Manager ownership/tracking follows later.
#![cfg_attr(target_arch = "riscv64", no_std)]
#![cfg_attr(target_arch = "riscv64", no_main)]
pub use mac_abi::definitions::{CDEF, ControlDefinitionState};
use mac_abi::{BAD_ABI, BAD_ARGUMENT, DefinitionCall, NOT_SUPPORTED};
use toolbox_quickdraw::{Point, SurfacePort, WHITE, font::CHICAGO_12};
#[derive(Clone, Copy)]
enum CaptionFont
{
    Classic(u32),
    GenevaBold(mac_abi::quickdraw::FontSelection)
}
impl CaptionFont
{
    fn width(self, canvas: &mut SurfacePort<'_>, text: &str) -> mac_traps::Result<i16>
    {
        match self
        {
            Self::Classic(font) => canvas.text_width(font, text),
            Self::GenevaBold(font) => canvas.font_width(font, text)
        }
    }

    fn metrics(self, canvas: &mut SurfacePort<'_>)
               -> mac_traps::Result<toolbox_quickdraw::FontMetrics>
    {
        // The original Geneva9 strike has no bold face. Resident synthetic
        // bold changes advance/ink horizontally; its vertical metrics match.
        canvas.font_metrics(match self
                            {
                                Self::Classic(font) => font,
                                Self::GenevaBold(_) => toolbox_quickdraw::font::GENEVA_9
                            })
    }

    fn draw(self, canvas: &mut SurfacePort<'_>, baseline: Point, text: &str, limit: i16,
            color: u32)
            -> mac_traps::Result<()>
    {
        match self
        {
            Self::Classic(font) =>
            {
                canvas.draw_text(font, baseline.h, baseline.v, text, limit, color)
            }
            Self::GenevaBold(font) =>
            {
                canvas.font_text_clipped(font, baseline, text, limit, color).map(|_| ())
            }
        }
    }
}
/// Standard CDEF0 push/check/radio definitions. Native message signatures are
/// explicit adapters; they are not the original Pascal CDEF calling convention.
/// # Safety
/// Call/state/surface storage must be live and exclusively borrowed.
pub unsafe extern "C" fn standard_control_definition(pointer: *mut DefinitionCall) -> i64
{
    let Some(call) = (unsafe { pointer.as_mut() })
    else
    {
        return BAD_ARGUMENT as i64;
    };
    if !call.header.supports::<DefinitionCall>()
    {
        return BAD_ABI as i64;
    }
    if call.kind != CDEF || call.id != 0 || !matches!(call.variation,0..=2|8..=10)
    {
        return NOT_SUPPORTED as i64;
    }
    let Some(state) = (unsafe { (call.object as *mut ControlDefinitionState).as_mut() })
    else
    {
        return BAD_ARGUMENT as i64;
    };
    match call.message
    {
        0 =>
        {
            let Some(surface) = (unsafe { call.surface.as_mut() })
            else
            {
                return BAD_ARGUMENT as i64;
            };
            let connection = unsafe { mac_traps::SurfaceConnection::new(surface, call.gate) };
            let mut canvas = SurfacePort::new(connection);
            draw(&mut canvas, state, call.variation).map_or_else(|e| e.status() as i64, |_| 0)
        }
        1 =>
        {
            let bits = call.param as u32;
            hit(state,
                call.variation,
                Point { v: (bits >> 16) as i16,
                        h: bits as i16 }) as i64
        }
        2..=4 => 0,
        mac_abi::controls::VALIDATE_DEFINITION =>
        {
            validate(state, call.variation).map_or_else(|e| e.status() as i64, |_| 0)
        }
        _ => NOT_SUPPORTED as i64
    }
}
/// Full control rectangle hit, including push-button rounded corners (original
/// CDEF0 offsets3f4..424). Hilite254 is a distinct original part result.
pub fn hit(state: &ControlDefinitionState, variation: i16, point: Point) -> i16
{
    if state.enabled == 0 || state.highlighted == 255 || !state.bounds.contains(point)
    {
        0
    }
    else if state.highlighted == 254
    {
        254
    }
    else if variation & 7 == 0
    {
        10
    }
    else
    {
        11
    }
}
/// CDEF0 original System7.6.1 algorithm, with bounded12×12 radio masks. The masks
/// encode only the glyph geometry; caption and arbitrary bounds remain dynamic.
pub fn draw(canvas: &mut SurfacePort<'_>, state: &ControlDefinitionState, variation: i16)
            -> mac_traps::Result<()>
{
    use toolbox_quickdraw::graphics::{PortState, Shape, Verb};
    let font = validate(state, variation)?;
    let r = state.bounds;
    let title=core::str::from_utf8(&state.title[..state.title_length as usize]).map_err(|_|mac_traps::Error::BadArgument)?;
    if matches!(font, CaptionFont::GenevaBold(_))
    {
        font.width(canvas, "")?;
    }
    let style = variation & 7;
    let disabled = state.enabled == 0 || state.highlighted >= 254;
    let pressed = !disabled && state.highlighted != 0;
    let mut port = PortState::default();
    if style == 0
    {
        let corner = ((i32::from(r.bottom) - i32::from(r.top)) / 2) as i16;
        let shape = Shape::RoundRect { bounds: r,
                                       corner: Point { h: corner,
                                                       v: corner } };
        port.foreground = if pressed { 0 } else { WHITE };
        canvas.draw_shape(&mut port, shape, Verb::Paint)?;
        port.foreground = 0;
        canvas.draw_shape(&mut port, shape, Verb::Frame)?;
    }
    else
    {
        canvas.paint_rect(r, WHITE)?;
        let top = i32::from(r.bottom) - ((i32::from(r.bottom) - i32::from(r.top) - 12) >> 1) - 12;
        let left = i32::from(r.left) + 2;
        if style == 1
        {
            let glyph =
                mac_abi::Rect::new(left as i16, top as i16, (left + 12) as i16, (top + 12) as i16);
            port.pen_size = Point { h: if pressed { 2 } else { 1 },
                                    v: if pressed { 2 } else { 1 } };
            canvas.draw_shape(&mut port, Shape::Rect(glyph), Verb::Frame)?;
            if state.value & 1 != 0
            {
                for n in 1..11
                {
                    pixel(canvas, left + n, top + n)?;
                    pixel(canvas, left + 11 - n, top + n)?;
                }
            }
        }
        else
        {
            let outline = if pressed { RADIO_PRESSED } else { RADIO };
            for (y, row) in outline.iter().enumerate()
            {
                for x in 0..12
                {
                    if row & (1 << (11 - x)) != 0
                    {
                        pixel(canvas, left + x, top + y as i32)?;
                    }
                }
            }
            if state.value & 1 != 0
            {
                for (y, row) in RADIO_DOT.iter().enumerate()
                {
                    for x in 0..12
                    {
                        if row & (1 << (11 - x)) != 0
                        {
                            pixel(canvas, left + x, top + y as i32)?;
                        }
                    }
                }
            }
        }
    }
    let metrics = font.metrics(canvas)?;
    let lines = title.split('\r').count() as i32;
    let line_height =
        i32::from(metrics.ascent) + i32::from(metrics.descent) + i32::from(metrics.leading);
    let mut baseline = i32::from(r.bottom)
                       - ((i32::from(r.bottom) - i32::from(r.top) - line_height * lines) >> 1)
                       - i32::from(metrics.leading)
                       - i32::from(metrics.descent);
    for line in title.split('\r')
    {
        let width = font.width(canvas, line)?;
        let x = if style == 0
        {
            i32::from(r.left) + ((i32::from(r.right) - i32::from(r.left) - i32::from(width)) >> 1)
        }
        else
        {
            i32::from(r.left) + 18
        };
        let limit = (i32::from(r.right) - x).clamp(0, i16::MAX as i32) as i16;
        font.draw(canvas,
                  Point { h: x as i16,
                          v: baseline as i16 },
                  line,
                  limit,
                  if disabled
                  {
                      0xa5a5a5
                  }
                  else if pressed && style == 0
                  {
                      WHITE
                  }
                  else
                  {
                      0
                  })?;
        baseline += line_height;
    }
    Ok(())
}
fn validate(state: &ControlDefinitionState, variation: i16) -> mac_traps::Result<CaptionFont>
{
    use toolbox_quickdraw::font::GENEVA_9;
    if !matches!(variation,0..=2|8..=10)
    {
        return Err(mac_traps::Error::NotSupported);
    }
    let r = state.bounds;
    let length = state.title_length as usize;
    if r.is_empty() || length > state.title.len()
    {
        return Err(mac_traps::Error::BadArgument);
    }
    core::str::from_utf8(&state.title[..length]).map_err(|_| mac_traps::Error::BadArgument)?;
    let font = if variation & 8 == 0
                  || (state.font.family == 0 && state.font.size == 12 && state.font.face == 0)
    {
        CaptionFont::Classic(CHICAGO_12)
    }
    else if matches!(state.font.family, 1 | 3) && state.font.size == 9 && state.font.face == 0
    {
        CaptionFont::Classic(GENEVA_9)
    }
    else if matches!(state.font.family, 1 | 3) && state.font.size == 9 && state.font.face == 1
    {
        CaptionFont::GenevaBold(state.font)
    }
    else
    {
        return Err(mac_traps::Error::NotSupported);
    };
    Ok(font)
}
fn pixel(canvas: &mut SurfacePort<'_>, x: i32, y: i32) -> mac_traps::Result<()>
{
    canvas.paint_rect(mac_abi::Rect::new(x as i16, y as i16, (x + 1) as i16, (y + 1) as i16), 0)
}
// Original CDEF0 FrameOval12×12 with1px/2px pen, and PaintOval inset3.
// Provenance: results/control-tracking-reference/static-{0,1,3}.png; resource
// SHA256 b647566a3fe540ae1e2477e6c3ced3d3a33cfffc131fcd8fbfa22c5774cc0dde.
const RADIO: [u16; 12] =
    [0x0f0, 0x30c, 0x402, 0x402, 0x801, 0x801, 0x801, 0x801, 0x402, 0x402, 0x30c, 0x0f0];
const RADIO_PRESSED: [u16; 12] =
    [0x0f0, 0x3fc, 0x70e, 0x606, 0xc03, 0xc03, 0xc03, 0xc03, 0x606, 0x70e, 0x3fc, 0x0f0];
const RADIO_DOT: [u16; 12] = [0, 0, 0, 0x0f0, 0x1f8, 0x1f8, 0x1f8, 0x1f8, 0x0f0, 0, 0, 0];
#[cfg(test)]
mod tests
{
    use mac_abi::{Header, Rect};

    use super::*;
    unsafe extern "C" fn gate(_: u32, _: *mut (), _: usize) -> i64
    {
        mac_abi::NOT_SUPPORTED as i64
    }
    #[test]
    fn button_hit_disabled_and_protocol_validation()
    {
        let mut state = ControlDefinitionState { bounds: Rect::new(10, 10, 80, 30),
                                                 title: [0; mac_abi::INLINE_STRING_BYTES],
                                                 title_length: 0,
                                                 enabled: 1,
                                                 highlighted: 0,
                                                 ..Default::default() };
        let mut call = DefinitionCall { header: Header::for_type::<DefinitionCall>(),
                                        kind: CDEF,
                                        id: 0,
                                        variation: 0,
                                        message: 1,
                                        reserved: 0,
                                        param: (15 << 16) | 15,
                                        object:
                                            (&mut state as *mut ControlDefinitionState).cast(),
                                        gate,
                                        surface: core::ptr::null_mut() };
        assert_eq!(unsafe { standard_control_definition(&mut call) }, 10);
        state.enabled = 0;
        assert_eq!(state.enabled, 0);
        assert_eq!(unsafe { standard_control_definition(&mut call) }, 0);
        call.message = 999;
        assert_eq!(unsafe { standard_control_definition(&mut call) }, NOT_SUPPORTED as i64);
        call.header.version = 99;
        assert_eq!(unsafe { standard_control_definition(&mut call) }, BAD_ABI as i64);
    }
}

/// # Safety
/// System supplies a live definition call and exclusive borrowed state/surface.
#[cfg(target_arch = "riscv64")]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(call: *mut mac_abi::DefinitionCall) -> i64
{
    unsafe { standard_control_definition(call) }
}
#[cfg(target_arch = "riscv64")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> !
{
    loop
    {
        core::hint::spin_loop();
    }
}
