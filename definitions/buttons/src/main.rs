//! Control definitions. Full Control Manager ownership/tracking follows later.
#![cfg_attr(target_arch = "riscv64", no_std)]
#![cfg_attr(target_arch = "riscv64", no_main)]
#![deny(unsafe_code)]
use mac_definition::{Button, ButtonKind, CaptionMode, Error, Highlight, Message, Part, Reply,
                     Result, Services};
use toolbox_quickdraw::{Point, SurfacePort, font::CHICAGO_12};
#[derive(Clone, Copy)]
enum CaptionFont
{
    Classic(toolbox_quickdraw::SurfaceFont),
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
#[mac_definition::cdef_main]
fn main<'a>(message: Message<'a, Button<'a>>, services: Services<'a>) -> Result<Reply>
{
    match message
    {
        Message::Reflow { request,
                          creation,
                          .. } =>
        {
            if services.reflow() != mac_definition::Reflow::Yes
            {
                return Err(Error::NotSupported);
            }
            return mac_definition::leaf_reflow(request,
                                               creation,
                                               mac_definition::reflow::Size::ZERO);
        }
        Message::Draw { state,
                        mut port } =>
        {
            if services.reflow() != mac_definition::Reflow::Yes || !button_bounds(&state).is_empty()
            {
                draw(&mut port, &state, services.theme()?)?;
            }
        }
        Message::HitTest { state,
                           point } => return Ok(Reply::Hit(hit(&state, point))),
        Message::Validate(state) =>
        {
            if services.reflow() == mac_definition::Reflow::Yes
            {
                caption_font(&state)?;
            }
            else
            {
                validate(&state)?;
            }
        }
        Message::CalculateRegions(_) | Message::Initialize(_) | Message::Dispose(_) => (),
        Message::Track { .. } | Message::ChangeBounds { .. } => return Err(Error::NotSupported)
    }
    Ok(Reply::Done)
}
/// Full control rectangle hit, including push-button rounded corners (original
/// CDEF0 offsets3f4..424). Hilite254 is a distinct original part result.
pub fn hit(state: &Button<'_>, point: Point) -> Part
{
    if !state.enabled
       || state.highlight == Highlight::Disabled
       || !button_bounds(state).contains(point)
    {
        Part::None
    }
    else if state.highlight == Highlight::Inactive
    {
        Part::Inactive
    }
    else if state.kind == ButtonKind::Push
    {
        Part::Button
    }
    else
    {
        Part::Check
    }
}
/// CDEF0 original System7.6.1 algorithm, with bounded12×12 radio masks. The masks
/// encode only the glyph geometry; caption and arbitrary bounds remain dynamic.
pub fn draw(canvas: &mut SurfacePort<'_>, state: &Button<'_>, theme: mac_definition::Theme)
            -> mac_traps::Result<()>
{
    use toolbox_quickdraw::graphics::{PortState, Shape, Verb};
    let font = validate(state)?;
    let r = button_bounds(state);
    let title = state.title;
    if matches!(font, CaptionFont::GenevaBold(_))
    {
        font.width(canvas, "")?;
    }
    let style = state.kind;
    let disabled =
        !state.enabled || matches!(state.highlight, Highlight::Inactive | Highlight::Disabled);
    let pressed = !disabled && state.highlight != Highlight::Normal;
    let mut port = PortState { foreground: theme.control_outline,
                               background: theme.content,
                               ..Default::default() };
    if style == ButtonKind::Push
    {
        if state.default_button
        {
            canvas.paint_rect(state.bounds, theme.content)?;
            port.pen_size = Point { h: 3,
                                    v: 3 };
            port.foreground = if disabled { theme.separator } else { theme.control_outline };
            canvas.draw_shape(&mut port,
                              Shape::RoundRect { bounds: state.bounds,
                                                 corner: Point { h: 16,
                                                                 v: 16 } },
                              Verb::Frame)?;
            port.pen_size = Point { h: 1,
                                    v: 1 };
        }
        let corner = ((i32::from(r.bottom) - i32::from(r.top)) / 2) as i16;
        let shape = Shape::RoundRect { bounds: r,
                                       corner: Point { h: corner,
                                                       v: corner } };
        port.foreground = if pressed { theme.control_outline } else { theme.content };
        canvas.draw_shape(&mut port, shape, Verb::Paint)?;
        port.foreground = theme.control_outline;
        canvas.draw_shape(&mut port, shape, Verb::Frame)?;
    }
    else
    {
        canvas.paint_rect(r, theme.content)?;
        let top = i32::from(r.bottom) - ((i32::from(r.bottom) - i32::from(r.top) - 12) >> 1) - 12;
        let left = i32::from(r.left) + 2;
        if style == ButtonKind::Check
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
                    pixel(canvas, left + n, top + n, theme.control_outline)?;
                    pixel(canvas, left + 11 - n, top + n, theme.control_outline)?;
                }
            }
        }
        else
        {
            draw_radio(canvas, left, top, pressed, state.value & 1 != 0, theme.control_outline)?;
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
        let x = if style == ButtonKind::Push
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
                      theme.disabled_text
                  }
                  else if pressed && style == ButtonKind::Push
                  {
                      theme.content
                  }
                  else
                  {
                      theme.text
                  })?;
        baseline += line_height;
    }
    Ok(())
}
fn validate(state: &Button<'_>) -> mac_traps::Result<CaptionFont>
{
    if button_bounds(state).is_empty()
    {
        return Err(Error::BadArgument);
    }
    caption_font(state)
}
fn caption_font(state: &Button<'_>) -> mac_traps::Result<CaptionFont>
{
    use toolbox_quickdraw::font::GENEVA_9;
    let font = if state.caption == CaptionMode::System
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
fn button_bounds(state: &Button<'_>) -> mac_abi::Rect
{
    let mut bounds = state.bounds;
    if state.kind == ButtonKind::Push && state.default_button
    {
        bounds.left = bounds.left.saturating_add(4);
        bounds.top = bounds.top.saturating_add(4);
        bounds.right = bounds.right.saturating_sub(4);
        bounds.bottom = bounds.bottom.saturating_sub(4);
    }
    bounds
}
fn draw_radio(canvas: &mut SurfacePort<'_>, left: i32, top: i32, pressed: bool, selected: bool,
              ink: u32)
              -> mac_traps::Result<()>
{
    let outline = if pressed { &RADIO_PRESSED } else { &RADIO };
    for (y, (&outline, &dot)) in outline.iter().zip(&RADIO_DOT).enumerate()
    {
        let row = outline | if selected { dot } else { 0 };
        let mut x = 0;
        while x < 12
        {
            if row & (1 << (11 - x)) == 0
            {
                x += 1;
                continue;
            }
            let start = x;
            while x < 12 && row & (1 << (11 - x)) != 0
            {
                x += 1;
            }
            let y = top + y as i32;
            canvas.paint_rect(mac_abi::Rect::new((left + start) as i16,
                                                 y as i16,
                                                 (left + x) as i16,
                                                 (y + 1) as i16),
                              ink)?;
        }
    }
    Ok(())
}
fn pixel(canvas: &mut SurfacePort<'_>, x: i32, y: i32, ink: u32) -> mac_traps::Result<()>
{
    canvas.paint_rect(mac_abi::Rect::new(x as i16, y as i16, (x + 1) as i16, (y + 1) as i16), ink)
}
// Original CDEF0 FrameOval12×12 with1px/2px pen, and PaintOval inset3.
// Provenance: results/control-tracking-reference/static-{0,1,3}.png; resource
// SHA256 b647566a3fe540ae1e2477e6c3ced3d3a33cfffc131fcd8fbfa22c5774cc0dde.
const RADIO: [u16; 12] =
    [0x0f0, 0x30c, 0x402, 0x402, 0x801, 0x801, 0x801, 0x801, 0x402, 0x402, 0x30c, 0x0f0];
const RADIO_PRESSED: [u16; 12] =
    [0x0f0, 0x3fc, 0x70e, 0x606, 0xc03, 0xc03, 0xc03, 0xc03, 0x606, 0x70e, 0x3fc, 0x0f0];
const RADIO_DOT: [u16; 12] = [0, 0, 0, 0x0f0, 0x1f8, 0x1f8, 0x1f8, 0x1f8, 0x0f0, 0, 0, 0];
