use mac_toolbox::{Rect, System,
                  controls::{Anchors, BorderOffsets, Cell, Fill, Reflow, SizeBounds, SizeUnit,
                             StaticTextSpec, TextAlignment, TextLayout},
                  events::{Event, EventMask},
                  include_resource_ids,
                  quickdraw::WHITE,
                  resources::APPLICATION,
                  windows::WindowSpec};



include_resource_ids!();


const QUIT: u32 = u32::from_be_bytes(*b"quit");

/// Keep the operation and typed Toolbox cause in the System-owned error report.
/// Graphical applications have no standard-stream endpoint.
struct Failure
{
    operation: &'static str,
    cause:     mac_toolbox::Error
}
impl Failure
{
    fn at(operation: &'static str, cause: mac_toolbox::Error) -> Self
    {
        Self { operation,
               cause }
    }
}
impl From<mac_toolbox::Error> for Failure
{
    fn from(cause: mac_toolbox::Error) -> Self
    {
        Self::at("Toolbox request", cause)
    }
}
impl std::fmt::Display for Failure
{
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result
    {
        write!(output, "Control Suite {}: {}", self.operation, self.cause)
    }
}
impl std::fmt::Debug for Failure
{
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result
    {
        std::fmt::Display::fmt(self, output)
    }
}
impl std::error::Error for Failure
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)>
    {
        Some(&self.cause)
    }
}

fn run(system: System) -> Result<(), Failure>
{
    system.menus().clear()?;

    system.menus().install_menu_resource(APPLICATION, 128)?;

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
        let (event, position) = system.wait_positioned(EventMask::ALL, 30)
                                      .map_err(|cause| Failure::at("event/layout wait", cause))?
                                      .into_parts();

        if let Some(selection) = system.menus()
                                       .selection(&event)
                                       .map_err(|cause| Failure::at("menu selection", cause))?
        {
            match selection.command
            {
                QUIT => return window.close().map_err(|cause| Failure::at("window close", cause)),

                _ => continue
            }
        }

        match event
        {
            Event::Update { window: target,
                            .. } if target == window.id() =>
            {
                let update = system.windows()
                                   .update(id)
                                   .map_err(|cause| Failure::at("begin window update", cause))?;
                let port = system.quickdraw().port(id);

                port.paint_rect(update.bounds(), WHITE)
                    .map_err(|cause| Failure::at("clear changed content", cause))?;
                table.control().draw().map_err(|cause| Failure::at("table drawing", cause))?;
                for label in labels
                {
                    label.draw().map_err(|cause| Failure::at("table child drawing", cause))?;
                }

                update.finish().map_err(|cause| Failure::at("finish window update", cause))?;
            }

            Event::MouseDown { window: Some(target),
                               .. } if target == id =>
            {
                let local =
                    position.in_window(id)
                            .map_err(|cause| Failure::at("capture window-local point", cause))?;
                let point =
                    local.point().map_err(|cause| Failure::at("read window-local point", cause))?;
                if let Some(hit) = grid.hit_test(point)
                                       .map_err(|cause| Failure::at("table child hit test", cause))?
                {
                    hit.control.track_at(local, |_| Ok(())).map_err(|cause| {
                                                                Failure::at("managed control \
                                                                             tracking",
                                                                            cause)
                                                            })?;
                }
            }

            Event::CloseRequested { window: target,
                                    .. } if target == window.id() =>
            {
                return window.close().map_err(|cause| Failure::at("window close", cause));
            }

            _ =>
            {}
        }
    }
}


fn main() -> Result<(), Failure>
{
    run(System::new()?)
}
