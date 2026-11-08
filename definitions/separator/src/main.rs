//! Passive, theme-coloured horizontal and vertical rules.
#![cfg_attr(systemrisc, no_std)]
#![cfg_attr(systemrisc, no_main)]
#![deny(unsafe_code)]
use mac_definition::{Error, Message, Part, Rect, Reply, Result, Separator, SeparatorOrientation,
                     Services, cdef_main};

#[cdef_main]
fn main<'a>(message: Message<'a, Separator>, services: Services<'a>) -> Result<Reply>
{
    match message
    {
        Message::Draw { state,
                        mut port } =>
        {
            let b = state.bounds;
            if !b.is_empty()
            {
                let theme = services.theme()?;
                // Paint only the rule. The retained surface owns background
                // clearing; untouched pixels and the port's clip stay intact.
                let line = match state.orientation
                {
                    SeparatorOrientation::Horizontal =>
                    {
                        let y = i32::from(b.top) + (i32::from(b.bottom) - i32::from(b.top)) / 2;
                        Rect::new(b.left, y as i16, b.right, (y + 1) as i16)
                    }
                    SeparatorOrientation::Vertical =>
                    {
                        let x = i32::from(b.left) + (i32::from(b.right) - i32::from(b.left)) / 2;
                        Rect::new(x as i16, b.top, (x + 1) as i16, b.bottom)
                    }
                };
                port.paint_rect(line, theme.separator)?;
            }
        }
        Message::HitTest { .. } => return Ok(Reply::Hit(Part::None)),
        Message::Reflow { request,
                          creation,
                          .. } =>
        {
            if services.reflow() != mac_definition::Reflow::Yes
            {
                return Err(Error::NotSupported);
            }
            // A visible one-pixel rule has no text or intrinsic long-axis
            // content. Layout may extend it or assign a clipped empty rect.
            let one = mac_definition::reflow::Extent::from_wide(1).map_err(|_| Error::BadArgument)?;
            return mac_definition::leaf_reflow(request,
                                               creation,
                                               mac_definition::reflow::Size::new(one, one));
        }
        Message::Validate(_)
        | Message::CalculateRegions(_)
        | Message::Initialize(_)
        | Message::Dispose(_) => (),
        Message::DrawGrid { .. }
        | Message::CalculateGridAxis(_)
        | Message::ChangeBounds { .. }
        | Message::Track { .. } =>
        {
            return Err(Error::NotSupported);
        }
    }
    Ok(Reply::Done)
}
