//! The actual Control Suite customer for every V2 stub. The original 2x2
//! splitter window remains independent. Table and Static Text own layout and
//! labels; the new families draw only their bounded rectangle and X.
use mac_toolbox::{Rect, System, ToolboxResult, WindowId,
                  controls::{Anchors, BorderOffsets, Cell, Fill, Reflow, Scrolling, SizeBounds,
                             SizeUnit, StaticTextSpec, Table, TextAlignment, TextLayout,
                             suite::{model, owned::Properties}},
                  events::WindowPoint,
                  windows::{Window, WindowSpec}};

pub(super) struct Gallery<'a>
{
    table:  Table<'a>,
    window: Window<'a>
}
fn size(minimum: i16, maximum: Option<i16>, fill: Fill) -> ToolboxResult<SizeBounds>
{
    SizeBounds::new(Some(SizeUnit::Pixels(minimum)), maximum.map(SizeUnit::Pixels), fill)
        .map_err(|e| mac_toolbox::Error::from_status(e.status()))
}
impl<'a> Gallery<'a>
{
    pub(super) fn open(system: System<'a>) -> ToolboxResult<Self>
    {
        let screen = system.quickdraw().screen_bounds()?;
        let top = i32::from(screen.top) + i32::from(system.menus().bar_height()?) + 40;
        let width = (i32::from(screen.right) - i32::from(screen.left) - 40).min(600);
        let height = (i32::from(screen.bottom) - top - 20).min(650);
        if width < 360 || height < 200
        {
            return Err(mac_toolbox::Error::NotSupported);
        }
        let left =
            i32::from(screen.left) + (i32::from(screen.right) - i32::from(screen.left) - width) / 2;
        let bounds =
            Rect::new(left as i16, top as i16, (left + width) as i16, (top + height) as i16);
        let window =
            system.windows()
                  .create_owned(WindowSpec::new(bounds, "Control Stubs").minimum(360, 200))?;
        window.set_close_deferred(true)?;
        let id = window.id();
        let table = system.controls().create_table(id,
                                                    Rect::new(12,
                                                              12,
                                                              width as i16 - 12,
                                                              height as i16 - 12),
                                                    Reflow::Yes)?;
        let grid = table.table();
        grid.set_column(0, size(180, Some(180), Fill::None)?)?;
        grid.set_column(1, size(100, None, Fill::Fill)?)?;
        grid.set_scrolling(Scrolling::Fixed { horizontal: false,
                                              vertical:   true })?;
        let mut row = 0;
        macro_rules! populate {
            ($(
             ($lt:lifetime,$name:ident,$tag:ident,$family:ident,$entry:ident,$constructor:ident,$properties:ident,$reference:ident,$cdef:literal,$variant:ident){
              $(($index:literal,$field:ident,$setter:ident,$marker:ident,[$ty:ty],$getter:ident,[$default:expr],$default_kind:ident)),*$(,)?
             }=>$validate:expr;
            )*) => {$({
                grid.set_row(row,size(24,Some(24),Fill::None)?)?;
                let label = system.controls().create_static_text_with(id,StaticTextSpec {
                    bounds:Rect::new(0,0,180,20),text:concat!(stringify!($name)," (",stringify!($cdef),")"),
                    font:Default::default(),alignment:TextAlignment::Left,layout:TextLayout::Ellipsis,reflow:Reflow::Yes
                })?;
                grid.set_child(Cell {row,column:0},label.into_control()).map_err(|e| e.cause)?;
                let properties = Properties::<model::$tag>::defaults().map_err(|e| match e {model::Error::NoMemory=>mac_toolbox::Error::NoMemory,_=>mac_toolbox::Error::BadArgument})?;
                let control = system.controls().$constructor(id,Rect::new(0,0,160,16),&properties,Reflow::Yes)?;
                grid.set_child(Cell {row,column:1},control.into_control()).map_err(|e| e.cause)?;
                row += 1;
            })*};
        }
        model::control_suite_inventory!(populate);
        // One real Tab control with tabs in it, since the inventory row above
        // has none. Reflow mode, so it owns its selection and draws its box.
        grid.set_row(row, size(90, Some(90), Fill::None)?)?;
        let label =
            system.controls()
                  .create_static_text_with(id,
                                           StaticTextSpec { bounds:    Rect::new(0, 0, 180, 20),
                                                            text:      "Tab, with tabs",
                                                            font:      Default::default(),
                                                            alignment: TextAlignment::Left,
                                                            layout:    TextLayout::Ellipsis,
                                                            reflow:    Reflow::Yes })?;
        grid.set_child(Cell { row,
                              column: 0 },
                       label.into_control())
            .map_err(|e| e.cause)?;
        let model_error = |e| {
            match e
            {
                model::Error::NoMemory => mac_toolbox::Error::NoMemory,
                model::Error::BadArgument => mac_toolbox::Error::BadArgument
            }
        };
        let properties = Properties::<model::TabModel>::defaults().map_err(model_error)?;
        let tabs = system.controls().create_tab(id,
                                                Rect::new(0, 0, 320, 90),
                                                &properties,
                                                Reflow::Yes)?;
        let titles = [model::TabItem { key:   1,
                                       title: "General",
                                       view:  model::Child::empty() },
                      model::TabItem { key:   2,
                                       title: "Sharing",
                                       view:  model::Child::empty() },
                      model::TabItem { key:   3,
                                       title: "Memory",
                                       view:  model::Child::empty() }];
        let list = mac_toolbox::controls::suite::owned::ListBuffer::from_slice(&titles)
            .map_err(model_error)?;
        tabs.set_tabs(list.view().map_err(model_error)?)?;
        tabs.set_selection(Some(1))?;
        grid.set_child(Cell { row,
                              column: 1 },
                       tabs.into_control())
            .map_err(|e| e.cause)?;
        row += 1;
        let _ = row;
        table.control().set_window_border(BorderOffsets::new(12,12,12,12).map_err(|e|mac_toolbox::Error::from_status(e.status()))?)?;
        table.control().set_anchors(Anchors::ALL)?;
        Ok(Self { table,
                  window })
    }

    pub(super) fn id(&self) -> WindowId
    {
        self.window.id()
    }

    pub(super) fn select(&self) -> ToolboxResult<()>
    {
        self.window.select()
    }

    pub(super) fn close(&mut self) -> ToolboxResult<()>
    {
        self.window.close()
    }

    pub(super) fn draw(&self, system: System<'a>) -> ToolboxResult<()>
    {
        let update = system.windows().update(self.id())?;
        system.quickdraw()
              .port(self.id())
              .paint_rect(update.bounds(), system.client().theme()?.content)?;
        self.table.control().draw()?;
        for child in self.table.table().children()
        {
            child?.control.draw()?;
        }
        update.finish()
    }

    pub(super) fn track(&self, point: WindowPoint) -> ToolboxResult<()>
    {
        if let Some(hit) = self.table.table().hit_test(point.point()?)?
        {
            hit.control.track_at(point, |_| Ok(()))?;
        }
        Ok(())
    }
}
