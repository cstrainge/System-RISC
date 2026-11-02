//! A divider owns its pixels and drag geometry. Its parent applies the result.
#![cfg_attr(target_arch = "riscv64", no_std)]
#![cfg_attr(target_arch = "riscv64", no_main)]
#![deny(unsafe_code)]
use mac_definition::{Error, Message, Part, Rect, Reply, Result, Services, Splitter, cdef_main};
#[cdef_main]
fn main<'a>(message: Message<'a, Splitter>, services: Services<'a>) -> Result<Reply>
{
    match message
    {
        Message::Draw { state,
                        mut port } =>
        {
            let theme = services.theme()?;
            port.paint_rect(state.bounds, theme.content)?;
            let b = state.bounds;
            if !b.is_empty()
            {
                let line = if state.horizontal
                {
                    let y = (i32::from(b.top) + i32::from(b.bottom)) / 2;
                    Rect::new(b.left, y as i16, b.right, (y + 1) as i16)
                }
                else
                {
                    let x = (i32::from(b.left) + i32::from(b.right)) / 2;
                    Rect::new(x as i16, b.top, (x + 1) as i16, b.bottom)
                };
                port.paint_rect(line,
                                if state.enabled
                                {
                                    theme.control_outline
                                }
                                else
                                {
                                    theme.separator
                                })?;
            }
        }
        Message::HitTest { state,
                           point } =>
        {
            return Ok(Reply::Hit(if state.enabled && state.bounds.contains(point)
                                 {
                                     Part::Splitter
                                 }
                                 else
                                 {
                                     Part::None
                                 }));
        }
        Message::Track { state,
                         anchor,
                         point } =>
        {
            let delta = if state.horizontal
            {
                i32::from(point.v) - i32::from(anchor.v)
            }
            else
            {
                i32::from(point.h) - i32::from(anchor.h)
            };
            let (min, max) = state.range.unwrap_or((i16::MIN, i16::MAX));
            let extent = if state.horizontal
            {
                i32::from(state.bounds.bottom) - i32::from(state.bounds.top)
            }
            else
            {
                i32::from(state.bounds.right) - i32::from(state.bounds.left)
            };
            let max = i32::from(max).min(i32::from(i16::MAX) - extent.max(0));
            let min = i32::from(min);
            if min > max
            {
                return Err(Error::BadArgument);
            }
            return Ok(Reply::Position((i32::from(state.position) + delta).clamp(min, max) as i16));
        }
        Message::Reflow { request,
                          creation,
                          .. } =>
        {
            return mac_definition::leaf_reflow(request,
                                               creation,
                                               mac_definition::reflow::Size::ZERO);
        }
        Message::Validate(_)
        | Message::CalculateRegions(_)
        | Message::Initialize(_)
        | Message::Dispose(_) => (),
        Message::CalculateGridAxis(_) | Message::DrawGrid { .. } | Message::ChangeBounds { .. } =>
        {
            return Err(Error::NotSupported);
        }
    }
    Ok(Reply::Done)
}
