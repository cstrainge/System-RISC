//! Resource viewers own the entire right-hand pane, including controls and input.
//! The browser supplies a source and a rectangle; it does not select viewer UI.
use core::fmt::Write;

use mac_toolbox::{controls::{ScrollBox, ScrollBoxRef, ScrollBoxSpec, scrollbox::View},
                  quickdraw::fonts::FontSelection,
                  resources::{FileResourceInfo, ResourceFile},
                  *};

use super::{FONT, HEX_FONT, document::Text};
mod code_view;
mod hex;
mod icon;
mod structured;
mod relocations;
#[derive(Clone, Copy)]
enum Kind
{
    Code,
    Hex,
    Icon,
    Strings,
    Menu,
    Version,
    Native,
    Relocations
}
pub enum Interaction
{
    None,
    Redraw,
    Scroll(Point)
}
struct Context<'s, 'a>
{
    system:  &'s System<'a>,
    window:  WindowId,
    file:    &'s ResourceFile<'a>,
    item:    FileResourceInfo,
    area:    Rect,
    control: ScrollBoxRef<'a>
}
impl Context<'_, '_>
{
    fn viewport(&self) -> Rect
    {
        Rect::new(self.area.left + 1,
                  self.area.top + 82,
                  self.area.right - 16,
                  self.area.bottom - 1)
    }

    fn scrollbar(&self) -> Rect
    {
        Rect::new(self.area.right - 16, self.area.top + 81, self.area.right, self.area.bottom)
    }

    fn spec(&self, count: u32, font: FontSelection) -> ScrollBoxSpec
    {
        ScrollBoxSpec { viewport: self.viewport(),
                        scrollbar: self.scrollbar(),
                        font,
                        padding: Point { h: 3,
                                         v: 11 },
                        row_height: 14,
                        count,
                        ..Default::default() }
    }

    fn prepare(&self, count: u32, font: FontSelection) -> ToolboxResult<View>
    {
        self.control.configure(self.spec(count, font))?;
        self.control.invalidate()?;
        self.control.info()
    }

    fn row(&self, index: u32, text: &str) -> ToolboxResult<()>
    {
        let mut length = text.len().min(768);
        while !text.is_char_boundary(length)
        {
            length -= 1;
        }
        self.control.set_row(index, &text[..length], false)
    }

    fn text(&self, x: i16, y: i16, text: &str) -> ToolboxResult<()>
    {
        self.system
            .quickdraw()
            .graphics(self.window)
            .text(FONT,
                  Point { h: self.area.left + x,
                          v: self.area.top + y },
                  text,
                  0)
            .map(|_| ())
    }

    fn label(&self, x: i16, y: i16, index: u32) -> ToolboxResult<()>
    {
        self.text(x, y, super::ui(self.system, index, &mut [0; 256])?)
    }

    fn header(&self, caption: bool) -> ToolboxResult<()>
    {
        let port = self.system.quickdraw().graphics(self.window);
        port.reset()?;
        port.clip_rect(self.area)?;
        if caption
        {
            self.label(3, 18, 5)?;
        }
        let mut meta = Text::<128>::new();
        meta.roman(&self.item.info.key.kind);
        let _ = write!(meta, "  ID {}  {} bytes", self.item.info.key.id, self.item.info.length);
        self.text(4, 37, meta.as_str())?;
        let mut meta = Text::<128>::new();
        let _ = write!(meta, "Attributes: 0x{:02x}", self.item.info.attributes);
        self.text(4, 52, meta.as_str())?;
        self.text(4, 67, if self.item.has_name { self.item.info.name() } else { "(unnamed)" })?;
        port.reset()
    }

    fn scroll_hit(&self, point: Point) -> Interaction
    {
        if self.scrollbar().contains(point)
        {
            Interaction::Scroll(point)
        }
        else
        {
            Interaction::None
        }
    }
}
trait Viewer
{
    fn draw(&mut self, ctx: &Context<'_, '_>) -> ToolboxResult<View>;
    fn fill(&mut self, ctx: &Context<'_, '_>, rows: core::ops::Range<u32>) -> ToolboxResult<()>;
    fn mouse(&mut self, ctx: &Context<'_, '_>, point: Point) -> ToolboxResult<Interaction>
    {
        Ok(ctx.scroll_hit(point))
    }

    fn key(&mut self, ctx: &Context<'_, '_>, key: u8) -> ToolboxResult<bool>
    {
        let info = ctx.control.info()?;
        let top = match key
                  {
                      0x79 => info.top.saturating_add(info.rows()),
                      0x7d => info.top.saturating_add(1),
                      0x74 => info.top.saturating_sub(info.rows()),
                      0x7e => info.top.saturating_sub(1),
                      0x73 => 0,
                      0x77 => info.maximum(),
                      _ => info.top
                  }.min(info.maximum());
        if top == info.top
        {
            return Ok(false);
        }
        let update = ctx.control.scroll_to(top)?;
        self.fill(ctx, update.missing)?;
        ctx.control.draw()?;
        Ok(true)
    }
    fn mode(&self) -> &'static str
    {
        "preview"
    }
}
pub struct ResourceView<'a>
{
    control:     ScrollBox<'a>,
    selected:    Option<ResourceKey>,
    kind:        Kind,
    code:        code_view::CodeViewer,
    hex:         hex::HexViewer,
    icon:        icon::IconViewer,
    strings:     structured::StringsViewer,
    menu:        structured::MenuViewer,
    version:     structured::VersionViewer,
    native:      structured::NativeViewer,
    relocations: relocations::RelocationsViewer
}
impl<'a> ResourceView<'a>
{
    pub fn new(system: &System<'a>, window: WindowId, area: Rect) -> ToolboxResult<Self>
    {
        let control = system.controls()
                            .create_scroll_box(window,
                                               ScrollBoxSpec { viewport: Rect::new(area.left + 1,
                                                                                   area.top + 82,
                                                                                   area.right - 16,
                                                                                   area.bottom - 1),
                                                               scrollbar: Rect::new(area.right - 16,
                                                                                    area.top + 81,
                                                                                    area.right,
                                                                                    area.bottom),
                                                               row_height: 14,
                                                               padding: Point { h: 3,
                                                                                v: 11 },
                                                               font: FONT,
                                                               ..Default::default() })?;
        Ok(Self { control,
                  selected: None,
                  kind: Kind::Hex,
                  code: Default::default(),
                  hex: hex::HexViewer,
                  icon: icon::IconViewer,
                  strings: structured::StringsViewer,
                  menu: structured::MenuViewer,
                  version: structured::VersionViewer,
                  native: structured::NativeViewer,
                  relocations: relocations::RelocationsViewer })
    }

    pub fn reset(&mut self)
    {
        self.selected = None;
        self.code = Default::default();
    }

    pub fn control(&self) -> ScrollBoxRef<'a>
    {
        self.control.control()
    }

    fn active(&mut self) -> &mut dyn Viewer
    {
        match self.kind
        {
            Kind::Code => &mut self.code,
            Kind::Hex => &mut self.hex,
            Kind::Icon => &mut self.icon,
            Kind::Strings => &mut self.strings,
            Kind::Menu => &mut self.menu,
            Kind::Version => &mut self.version,
            Kind::Native => &mut self.native,
            Kind::Relocations => &mut self.relocations
        }
    }

    fn context<'s>(&self, system: &'s System<'a>, window: WindowId, file: &'s ResourceFile<'a>,
                   item: FileResourceInfo, area: Rect)
                   -> Context<'s, 'a>
    {
        Context { system,
                  window,
                  file,
                  item,
                  area,
                  control: self.control() }
    }

    fn report(&mut self, ctx: &Context<'_, '_>) -> ToolboxResult<()>
    {
        let mut log = Text::<128>::new();
        let _ = writeln!(log,
                         "RESEDIT_RESOURCE kind={} id={} bytes={} top={} mode={}",
                         u32::from_be_bytes(ctx.item.info.key.kind),
                         ctx.item.info.key.id,
                         ctx.item.info.length,
                         ctx.control.info()?.top,
                         self.active().mode());
        ctx.system.log(log.as_str()).map_err(Error)
    }

    pub fn draw(&mut self, system: &System<'a>, window: WindowId, file: Option<&ResourceFile<'a>>,
                item: Option<FileResourceInfo>, area: Rect)
                -> ToolboxResult<View>
    {
        let (Some(file), Some(item)) = (file, item)
        else
        {
            super::text(system,
                        window,
                        area.left + 3,
                        area.top + 18,
                        super::ui(system, 5, &mut [0; 128])?,
                        0)?;
            let mut spec = self.control().info()?.spec;
            spec.viewport =
                Rect::new(area.left + 1, area.top + 82, area.right - 16, area.bottom - 1);
            spec.scrollbar = Rect::new(area.right - 16, area.top + 81, area.right, area.bottom);
            spec.count = 0;
            self.control().configure(spec)?;
            self.control().invalidate()?;
            self.control().draw()?;
            return self.control().info();
        };
        if self.selected != Some(item.info.key)
        {
            self.control().scroll_to(0)?;
            self.selected = Some(item.info.key);
        }
        self.kind = match &item.info.key.kind
        {
            b"RV64" | b"CODE" => Kind::Code,
            b"RLOC" => Kind::Relocations,
            b"MENU" if item.info.length <= 32768 => Kind::Menu,
            b"STR " | b"STR#" if item.info.length <= 32768 => Kind::Strings,
            b"vers" if item.info.length <= 32768 => Kind::Version,
            b"NABI" if item.info.length <= 32768 => Kind::Native,
            b"ICON" | b"ICN#" | b"ics#" | b"SICN" | b"cicn" | b"icl4" | b"icl8" | b"ics4"
            | b"ics8"
                if item.info.length <= 32768 =>
            {
                Kind::Icon
            }
            _ => Kind::Hex
        };
        let ctx = self.context(system, window, file, item, area);
        let view = self.active().draw(&ctx)?;
        self.report(&ctx)?;
        Ok(view)
    }

    pub fn mouse(&mut self, system: &System<'a>, window: WindowId, file: &ResourceFile<'a>,
                 item: FileResourceInfo, area: Rect, point: Point)
                 -> ToolboxResult<bool>
    {
        let ctx = self.context(system, window, file, item, area);
        match self.active().mouse(&ctx, point)?
        {
            Interaction::None => Ok(false),
            Interaction::Redraw => Ok(true),
            Interaction::Scroll(anchor) =>
            {
                ctx.control.track(anchor, |view| {
                                let started = system.events().monotonic_nanoseconds()?;
                                let range = view.missing.clone();
                                self.active().fill(&ctx, range.clone())?;
                                let elapsed = system.events()
                                                    .monotonic_nanoseconds()?
                                                    .saturating_sub(started);
                                let mut log = Text::<128>::new();
                                let _ = writeln!(log,
                                                 "RESEDIT_ROWS pane=2 first={} end={} \
                                                  ns={elapsed}",
                                                 range.start, range.end);
                                system.log(log.as_str()).map_err(Error)
                            })?;
                self.report(&ctx)?;
                Ok(false)
            }
        }
    }

    pub fn key(&mut self, system: &System<'a>, window: WindowId, file: &ResourceFile<'a>,
               item: FileResourceInfo, area: Rect, key: u8)
               -> ToolboxResult<()>
    {
        let ctx = self.context(system, window, file, item, area);
        let started = system.events().monotonic_nanoseconds()?;
        if self.active().key(&ctx, key)?
        {
            self.report(&ctx)?;
            let elapsed = system.events().monotonic_nanoseconds()?.saturating_sub(started);
            let mut log = Text::<128>::new();
            let _ = writeln!(log, "RESEDIT_RENDER pane=2 ns={elapsed}");
            system.log(log.as_str()).map_err(Error)?;
        }
        Ok(())
    }
}
