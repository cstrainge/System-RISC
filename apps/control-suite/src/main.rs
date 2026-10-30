#![no_std]
#![no_main]

use mac_toolbox::{Rect, System, SystemServices, ToolboxResult,
                  abi::{OK, Status},
                  controls::{Anchors, BorderOffsets, Cell, Fill, Reflow, SizeBounds, SizeUnit,
                             StaticTextSpec, TextAlignment, TextLayout},
                  events::{Event, EventMask},
                  include_resource_ids,
                  panic::install,
                  panic_handler,
                  quickdraw::WHITE,
                  resources::APPLICATION,
                  windows::WindowSpec};



include_resource_ids!();


const QUIT: u32 = u32::from_be_bytes(*b"quit");


fn run(system: System) -> ToolboxResult<()>
{
    system.menus().clear()?;

    let _menu = [system.menus().install_owned(APPLICATION, 128)?];

    let mut window = system.windows().create_owned(
            WindowSpec::new(Rect::new(80, 80, 480, 320), "Control Suite")
                .minimum(200, 200)
                .procedure(0))?;

    let id = window.id();
    window.set_close_deferred(true)?;

    let table = system.controls().create_table(id, Rect::new(12, 12, 388, 228), Reflow::Yes)?;
    let grid = table.table();
    let columns=SizeBounds::new(Some(SizeUnit::Pixels(80)),None,Fill::Fill).map_err(|e|mac_toolbox::Error::from_status(e.status()))?;
    let rows=SizeBounds::new(Some(SizeUnit::Pixels(60)),None,Fill::Fill).map_err(|e|mac_toolbox::Error::from_status(e.status()))?;
    for index in 0..2
    {
        grid.set_column(index, columns)?;
        grid.set_row(index, rows)?;
    }
    grid.set_column_splitter(0, true)?;
    grid.set_row_splitter(0, true)?;
    let mut labels = [table.control(); 4];
    for (index, text) in
        ["Top left", "Top right", "Bottom left", "Bottom right"].into_iter().enumerate()
    {
        let label=system.controls().create_static_text_with(id,StaticTextSpec {
            bounds:Rect::new(0,0,120,40),text,font:Default::default(),alignment:TextAlignment::Center,
            layout:TextLayout::Wrap,reflow:Reflow::Yes })?;
        labels[index] = grid.set_child(Cell { row:    (index / 2) as u32,
                                              column: (index % 2) as u32 },
                                       label.into_control())
                            .map_err(|e| e.cause)?;
    }
    table.control().set_window_border(BorderOffsets::new(12,12,12,12).map_err(|e|mac_toolbox::Error::from_status(e.status()))?)?;
    table.control().set_anchors(Anchors::ALL)?;

    loop
    {
        let (event, position) = system.wait_positioned(EventMask::ALL, 30)?.into_parts();

        if let Some(selection) = system.menus().selection(&event)?
        {
            match selection.command
            {
                QUIT => return window.close(),

                _ => continue
            }
        }

        match event
        {
            Event::Update { window: target,
                            .. } if target == window.id() =>
            {
                let update = system.windows().update(id)?;
                let port = system.quickdraw().port(id);

                port.paint_rect(update.bounds(), WHITE)?;
                table.control().draw()?;
                for label in labels
                {
                    label.draw()?;
                }

                update.finish()?;
            }

            Event::MouseDown { window: Some(target),
                               .. } if target == id =>
            {
                let local = position.in_window(id)?;
                if let Some(hit) = grid.hit_test(local.point()?)?
                {
                    hit.control.track_at(local, |_| Ok(()))?;
                }
            }

            Event::CloseRequested { window: target,
                                    .. } if target == window.id() =>
            {
                return window.close();
            }

            _ =>
            {}
        }
    }
}


#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> Status
{
    if let Err(status) = unsafe { install(services) }
    {
        return status;
    }

    let system = match unsafe { System::from_raw(services) }
    {
        Ok(system) => system,
        Err(status) => return status.status()
    };

    match run(system)
    {
        Ok(()) => OK,
        Err(status) => status.status()
    }
}


panic_handler!();
