//! Independently relocatable static-text CDEF. Author code is safe Rust; the
//! platform supplies decoded calls, exclusive ports, entry and panic reporting.
#![cfg_attr(target_arch = "riscv64", no_std)]
#![cfg_attr(target_arch = "riscv64", no_main)]
#![deny(unsafe_code)]
use mac_definition::{Alignment, Error, Message, Part, Point, Reply, Result, Services, StaticText,
                     SurfacePort};

fn draw(canvas: &mut SurfacePort<'_>, state: StaticText<'_>, text_color: u32) -> Result<()>
{
    let r = state.bounds;
    // Classic static text is transparent. The retained parent restores old ink
    // through the control's bounded damage, preserving its actual background.
    let metrics = canvas.selected_font_metrics(state.font)?;
    let baseline = i32::from(r.top) + i32::from(metrics.ascent);
    let line_height = (i32::from(metrics.ascent)
                       + i32::from(metrics.descent)
                       + i32::from(metrics.leading)).max(1);
    for (row, line) in state.text
                            .split('\n')
                            .flat_map(|line| line.strip_suffix('\r').unwrap_or(line).split('\r'))
                            .enumerate()
    {
        let y = baseline + row as i32 * line_height;
        if y - i32::from(metrics.ascent) >= i32::from(r.bottom)
        {
            break;
        }
        if line.is_empty()
        {
            continue;
        }
        let width = i32::from(canvas.font_width(state.font, line)?);
        let available = i32::from(r.right) - i32::from(r.left);
        let x = i32::from(r.left)
                + match state.alignment
                {
                    Alignment::Right => (available - width).max(0),
                    Alignment::Center => ((available - width) / 2).max(0),
                    Alignment::Left => 0
                };
        canvas.font_text_clipped(state.font,
                                 Point { h: x as i16,
                                         v: y.min(i16::MAX as i32) as i16 },
                                 line,
                                 (i32::from(r.right) - x).min(i16::MAX as i32) as i16,
                                 text_color)?;
    }
    Ok(())
}
#[mac_definition::cdef_main]
fn main<'a>(message: Message<'a, StaticText<'a>>, services: Services<'a>) -> Result<Reply>
{
    match message
    {
        Message::DrawGrid { .. } | Message::CalculateGridAxis(_) =>
        {
            return Err(Error::NotSupported);
        }
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
            let theme = services.theme()?;
            draw(&mut port, state, if state.enabled { theme.text } else { theme.disabled_text })?;
        }
        Message::Validate(state) =>
        {
            services.font_metrics(state.font)?;
        }
        Message::HitTest { .. } => return Ok(Reply::Hit(Part::None)),
        Message::CalculateRegions(_) | Message::Initialize(_) | Message::Dispose(_) => (),
        Message::ChangeBounds { .. } | Message::Track { .. } => return Err(Error::NotSupported)
    }
    Ok(Reply::Done)
}
