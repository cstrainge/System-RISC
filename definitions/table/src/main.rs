//! Table definition bootstrap: a passive dotted grid, with no data or interaction yet.
#![cfg_attr(target_arch = "riscv64", no_std)]
#![cfg_attr(target_arch = "riscv64", no_main)]
#![deny(unsafe_code)]

use mac_definition::{Error, Message, Part, Rect, Reply, Result, Services, Table, cdef_main};



#[cdef_main]
fn main<'a>(message: Message<'a, Table>, services: Services<'a>) -> Result<Reply>
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
            let theme = services.theme()?;
            let bounds = state.bounds;
            let color = if state.enabled { theme.control_outline } else { theme.separator };
            let middle_x = (i32::from(bounds.left)
                            + (i32::from(bounds.right) - i32::from(bounds.left)) / 2)
                           as i16;
            let middle_y = (i32::from(bounds.top)
                            + (i32::from(bounds.bottom) - i32::from(bounds.top)) / 2)
                           as i16;
            // A checker pattern makes both horizontal and vertical strokes dotted.
            // Six narrow fills keep drawing bounded without one call per dot.
            let dots = [0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55];
            port.paint_rect(bounds, theme.content)?;
            for line in [Rect::new(bounds.left, bounds.top, bounds.right, bounds.top + 1),
                         Rect::new(bounds.left, bounds.bottom - 1, bounds.right, bounds.bottom),
                         Rect::new(bounds.left, bounds.top, bounds.left + 1, bounds.bottom),
                         Rect::new(bounds.right - 1, bounds.top, bounds.right, bounds.bottom),
                         Rect::new(bounds.left, middle_y, bounds.right, middle_y + 1),
                         Rect::new(middle_x, bounds.top, middle_x + 1, bounds.bottom)]
            {
                port.fill_rect(line, dots, color, theme.content)?;
            }
        }
        Message::HitTest { .. } => return Ok(Reply::Hit(Part::None)),
        Message::Validate(_)
        | Message::CalculateRegions(_)
        | Message::Initialize(_)
        | Message::Dispose(_) => (),
        Message::Track { .. } | Message::ChangeBounds { .. } => return Err(Error::NotSupported)
    }

    Ok(Reply::Done)
}
