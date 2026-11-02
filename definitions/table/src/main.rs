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
    if state.lines.border && !state.bounds.is_empty()
    {
        let b = state.bounds;
        for line in [Rect::new(b.left, b.top, b.right, b.top + 1),
                     Rect::new(b.left, b.bottom - 1, b.right, b.bottom),
                     Rect::new(b.left, b.top, b.left + 1, b.bottom),
                     Rect::new(b.right - 1, b.top, b.right, b.bottom)]
        {
            port.fill_rect(line, dots, color, theme.content)?;
        }
    }
    if !state.lines.rows && !state.lines.columns
    {
        return Ok(());
    }
    for bounds in cells
    {
        if bounds.is_empty()
        {
            continue;
        }
        // Outer cell edges must not recreate a disabled Table border. Complete
        // merged regions have no interior edges, so no line crosses a span.
        if state.lines.rows
        {
            if bounds.top > state.bounds.top
            {
                port.fill_rect(Rect::new(bounds.left, bounds.top, bounds.right, bounds.top + 1),
                               dots,
                               color,
                               theme.content)?;
            }
            if bounds.bottom < state.bounds.bottom
            {
                port.fill_rect(Rect::new(bounds.left,
                                         bounds.bottom - 1,
                                         bounds.right,
                                         bounds.bottom),
                               dots,
                               color,
                               theme.content)?;
            }
        }
        if state.lines.columns
        {
            if bounds.left > state.bounds.left
            {
                port.fill_rect(Rect::new(bounds.left, bounds.top, bounds.left + 1, bounds.bottom),
                               dots,
                               color,
                               theme.content)?;
            }
            if bounds.right < state.bounds.right
            {
                port.fill_rect(Rect::new(bounds.right - 1,
                                         bounds.top,
                                         bounds.right,
                                         bounds.bottom),
                               dots,
                               color,
                               theme.content)?;
            }
        }
    }
    Ok(())
}
