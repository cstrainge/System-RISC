//! Table owns grid allocation policy; System owns child lifetimes and publication.
#![cfg_attr(target_arch = "riscv64", no_std)]
#![cfg_attr(target_arch = "riscv64", no_main)]
#![deny(unsafe_code)]

mod layout;

use mac_definition::{Error, Message, Part, Rect, Reply, Result, Services, Table, cdef_main};



#[cdef_main]
fn main<'a>(message: Message<'a, Table>, services: Services<'a>) -> Result<Reply>
{
    match message
    {
        Message::CalculateGridAxis(mut calculation) =>
        {
            layout::calculate(&mut calculation).map_err(|e| Error::from_status(e.status()))?;
            return Ok(Reply::Done);
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
                        port } => draw(state, port, &[], services)?,
        Message::DrawGrid { state,
                            port,
                            cells } => draw(state, port, cells, services)?,
        Message::HitTest { .. } => return Ok(Reply::Hit(Part::None)),
        Message::Validate(_)
        | Message::CalculateRegions(_)
        | Message::Initialize(_)
        | Message::Dispose(_) => (),
        Message::Track { .. } | Message::ChangeBounds { .. } => return Err(Error::NotSupported)
    }

    Ok(Reply::Done)
}

fn draw(state: Table, mut port: mac_definition::SurfacePort<'_>, cells: &[Rect],
        services: Services<'_>)
        -> Result<()>
{
    let theme = services.theme()?;
    let color = if state.enabled { theme.control_outline } else { theme.separator };
    let dots = [0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55, 0xaa, 0x55];
    port.paint_rect(state.bounds, theme.content)?;
    for bounds in core::iter::once(&state.bounds).chain(cells)
    {
        if bounds.is_empty()
        {
            continue;
        }
        // Valid nonempty endpoints make each +/-1 safe, including edge pixels.
        for line in [Rect::new(bounds.left, bounds.top, bounds.right, bounds.top + 1),
                     Rect::new(bounds.left, bounds.bottom - 1, bounds.right, bounds.bottom),
                     Rect::new(bounds.left, bounds.top, bounds.left + 1, bounds.bottom),
                     Rect::new(bounds.right - 1, bounds.top, bounds.right, bounds.bottom)]
        {
            port.fill_rect(line, dots, color, theme.content)?;
        }
    }
    Ok(())
}
