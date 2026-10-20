//! ResEdit read-only resource browser with lossless native JSON export.
#![no_std]
#![no_main]
use core::fmt::{self, Write};

use mac_toolbox::{files::{CatalogInfo, ROOT_ID},
                  quickdraw::{cursors::StandardCursor, fonts::FontSelection},
                  resources::{FileResourceInfo, ResourceFile},
                  *};
mod code;
mod document;
mod preview;
mod view;
mod viewers;
use controls::scrollbox::View;
use document::Text;
use view::{Layout, Pane};
const FONT: FontSelection = FontSelection { family: 3,
                                            size:   9,
                                            face:   0 };
const HEX_FONT: FontSelection = FontSelection { family: 4,
                                                size:   9,
                                                face:   0 };


fn ui<'b>(system: &System<'_>, index: u32, buffer: &'b mut [u8]) -> ToolboxResult<&'b str>
{
    system.resources().application().indexed_string(2000, index, buffer)
}
fn text(system: &System<'_>, window: WindowId, x: i16, y: i16, value: &str, color: u32)
        -> ToolboxResult<()>
{
    system.quickdraw()
          .graphics(window)
          .text(FONT,
                Point { h: x,
                        v: y },
                value,
                color)
          .map(|_| ())
}
fn label(system: &System<'_>, window: WindowId, x: i16, y: i16, index: u32) -> ToolboxResult<()>
{
    text(system, window, x, y, ui(system, index, &mut [0; 256])?, 0)
}
fn window_layout(system: &System<'_>) -> ToolboxResult<(Rect, i16)>
{
    let resource = system.resources().application().get(ResourceKey { kind: *b"WIND",
                                                                       id:   2003 })?;
    let mut data = [0; 20];
    let size = resource.read(0, &mut data)?;
    resource.release()?;
    if size != data.len()
    {
        return Err(Error::BadArgument);
    }
    let word = |at| i16::from_be_bytes([data[at], data[at + 1]]);
    let bounds = Rect::new(word(2), word(0), word(6), word(4));
    if bounds.right.checked_sub(bounds.left).is_none_or(|n| n < 600)
       || bounds.bottom.checked_sub(bounds.top).is_none_or(|n| n < 400)
       || word(8) != 0
    {
        return Err(Error::BadArgument);
    }
    Ok((bounds, word(8)))
}
struct Browser<'a>
{
    boxes:          [controls::ScrollBox<'a>; 2],
    resource_view:  viewers::ResourceView<'a>,
    preview_rows:   u32,
    selected:       Option<FileResourceInfo>,
    catalog:        Option<CatalogInfo>,
    file:           Option<ResourceFile<'a>>,
    type_index:     u32,
    resource_index: u32,
    type_top:       u32,
    resource_top:   u32,
    preview_top:    u32,
    type_count:     u32,
    resource_count: u32,
    preview_count:  u32,
    saved:          Option<standard_file::Selection>,
    volume:         u32,
    directory:      u32,
    status:         u32,
    export_number:  u32
}
impl<'a> Browser<'a>
{
    fn new(system: &System<'a>, window: WindowId) -> ToolboxResult<Self>
    {
        let volume = system.files().volume_at(0)?.id;
        let directory =
            system.files().lookup(volume, ROOT_ID, "Applications").map(|e| e.id).unwrap_or(ROOT_ID);
        let bounds = system.windows().info(window)?.bounds;
        let layout = Layout { width:  bounds.right - bounds.left,
                              height: bounds.bottom - bounds.top };
        let create =
            |pane| system.controls().create_scroll_box(window, Self::spec(layout, pane, 0, FONT));
        Ok(Self { boxes: [create(Pane::Types)?, create(Pane::Resources)?],
                  resource_view: viewers::ResourceView::new(system,
                                                            window,
                                                            Self::resource_area(layout))?,
                  preview_rows: layout.rows(Pane::Preview),
                  selected: None,
                  catalog: None,
                  file: None,
                  type_index: 0,
                  resource_index: 0,
                  type_top: 0,
                  resource_top: 0,
                  preview_top: 0,
                  type_count: 0,
                  resource_count: 0,
                  preview_count: 0,
                  saved: None,
                  volume,
                  directory,
                  status: 9,
                  export_number: 0 })
    }

    fn open(&mut self, system: &System<'a>, window: WindowId, item: CatalogInfo)
            -> ToolboxResult<()>
    {
        let file = system.resources().open_file(item.volume, item.id)?;
        system.windows().set_title(window, item.name())?;
        self.file = Some(file);
        self.catalog = Some(item);
        self.type_index = 0;
        self.resource_index = 0;
        self.type_top = 0;
        self.resource_top = 0;
        self.preview_top = 0;
        self.resource_view.reset();
        self.saved = None;
        self.status = 10;
        let mut log = Text::<256>::new();
        let _ = writeln!(log,
                         "RESEDIT_OPEN volume={} cnid={} name={}",
                         item.volume,
                         item.id,
                         item.name());
        system.log(log.as_str())
    }

    fn draw(&mut self, system: &System<'a>, window: WindowId) -> ToolboxResult<()>
    {
        let started = system.events().monotonic_nanoseconds()?;
        let bounds = system.windows().info(window)?.bounds;
        let width = bounds.right - bounds.left;
        let height = bounds.bottom - bounds.top;
        system.windows().set_grow_icon_clip(window,
                                             Some(Rect::new(width - 15,
                                                            height - 15,
                                                            width,
                                                            height)))?;
        system.windows().begin_update(window)?;
        let port = system.quickdraw().graphics(window);
        port.reset()?;
        port.colors(0xffffff, 0xffffff)?;
        port.paint_rect(Rect::new(0, 0, width, height))?;
        port.colors(0, 0xffffff)?;
        self.draw_resources(system, window, width, height)?;
        port.clip_rect(Rect::new(4, height - 26, width - 18, height - 3))?;
        label(system, window, 10, height - 10, self.status)?;
        port.reset()?;
        system.windows().end_update(window)?;
        let elapsed = system.events().monotonic_nanoseconds()?.saturating_sub(started);
        let mut timing = Text::<128>::new();
        let _ = writeln!(timing, "RESEDIT_RENDER pane=all ns={elapsed}");
        system.log(timing.as_str())?;
        system.log("RESEDIT_DRAW_READY\n")
    }

    fn control(&self, pane: Pane) -> controls::ScrollBoxRef<'a>
    {
        if pane == Pane::Preview
        {
            self.resource_view.control()
        }
        else
        {
            self.boxes[pane as usize].control()
        }
    }

    fn spec(layout: Layout, pane: Pane, count: u32, font: FontSelection)
            -> controls::ScrollBoxSpec
    {
        let body = layout.body(pane);
        controls::ScrollBoxSpec { viewport: body,
                                  scrollbar: layout.scrollbar(pane),
                                  font,
                                  padding: Point { h: if pane == Pane::Preview { 3 } else { 5 },
                                                   v: layout.baseline(pane, 0) - body.top },
                                  row_height: layout.row_height(pane),
                                  count,
                                  ..Default::default() }
    }

    fn set_top(&mut self, pane: Pane, top: u32)
    {
        match pane
        {
            Pane::Types => self.type_top = top,
            Pane::Resources => self.resource_top = top,
            Pane::Preview => self.preview_top = top
        }
    }

    fn row(control: controls::ScrollBoxRef<'_>, index: u32, text: &str, selected: bool)
           -> ToolboxResult<()>
    {
        let mut length = text.len().min(768);
        while !text.is_char_boundary(length)
        {
            length -= 1;
        }
        control.set_row(index, &text[..length], selected)
    }

    fn fill_rows(&mut self, _system: &System<'a>, _window: WindowId, _layout: Layout, pane: Pane,
                 view: View)
                 -> ToolboxResult<()>
    {
        self.set_top(pane, view.top);

        let control = self.control(pane);
        let Some(file) = &self.file
        else
        {
            return Ok(());
        };
        let kind = self.selected.map(|item| item.info.key.kind).unwrap_or(*b"    ");
        for index in view.missing
        {
            let mut line = Text::<1024>::new();
            let selected = if pane == Pane::Types
            {
                let kind = file.type_at(index)?;
                line.roman(&kind);
                let _ = write!(line, " ({})", file.count(kind)?);
                index == self.type_index
            }
            else
            {
                let item = file.at(kind, index)?;
                let _ = write!(line, "{}  {}", item.info.key.id, item.info.name());
                index == self.resource_index
            };
            Self::row(control, index, line.as_str(), selected)?;
        }
        Ok(())
    }

    fn track_scrollbar(&mut self, system: &System<'a>, window: WindowId, layout: Layout,
                       pane: Pane, anchor: Point)
                       -> ToolboxResult<()>
    {
        let control = self.control(pane);
        control.track(anchor, |view| {
                   let started = system.events().monotonic_nanoseconds()?;
                   let first = view.missing.start;
                   let end = view.missing.end;
                   self.fill_rows(system, window, layout, pane, view)?;
                   let elapsed = system.events().monotonic_nanoseconds()?.saturating_sub(started);
                   let mut log = Text::<128>::new();
                   let _ = writeln!(log,
                                    "RESEDIT_ROWS pane={} first={first} end={end} ns={elapsed}",
                                    pane as u32);
                   system.log(log.as_str())
               })?;
        self.set_top(pane, control.info()?.top);
        self.report(system, layout)?;
        system.log("RESEDIT_DRAW_READY\n")
    }

    fn report(&self, system: &System<'a>, layout: Layout) -> ToolboxResult<()>
    {
        let mut log = Text::<256>::new();
        let _ = writeln!(log,
                         "RESEDIT_VIEW types_top={} types_rows={} resources_top={} \
                          resources_rows={} preview_top={} preview_rows={} preview_max={}",
                         self.type_top,
                         layout.rows(Pane::Types),
                         self.resource_top,
                         layout.rows(Pane::Resources),
                         self.preview_top,
                         self.preview_rows,
                         self.preview_count.saturating_sub(self.preview_rows));
        system.log(log.as_str())
    }

    fn draw_resources(&mut self, system: &System<'a>, window: WindowId, width: i16, height: i16)
                      -> ToolboxResult<()>
    {
        let layout = Layout { width,
                              height };
        let port = system.quickdraw().graphics(window);
        label(system, window, 10, 18, 2)?;
        label(system, window, 120, 18, 3)?;

        for pane in [Pane::Types, Pane::Resources]
        {
            port.frame_rect(layout.bounds(pane))?;
        }
        self.type_count = if let Some(file) = &self.file { file.type_count()? } else { 0 };
        let kind = if self.type_count == 0
        {
            *b"    "
        }
        else
        {
            self.file.as_ref().unwrap().type_at(self.type_index)?
        };
        self.resource_count =
            if self.type_count == 0 { 0 } else { self.file.as_ref().unwrap().count(kind)? };
        self.selected = if self.resource_count == 0
        {
            None
        }
        else
        {
            Some(self.file.as_ref().unwrap().at(kind, self.resource_index)?)
        };
        for (pane, count, top) in [(Pane::Types, self.type_count, self.type_top),
                                   (Pane::Resources, self.resource_count, self.resource_top)]
        {
            let control = self.control(pane);
            control.configure(Self::spec(layout, pane, count, FONT))?;
            control.invalidate()?;
            let view = control.scroll_to(top)?;
            self.fill_rows(system, window, layout, pane, view)?;
            control.draw()?;
        }
        let view = self.resource_view.draw(system,
                                            window,
                                            self.file.as_ref(),
                                            self.selected,
                                            Self::resource_area(layout))?;
        self.preview_top = view.top;
        self.preview_count = view.spec.count;
        self.preview_rows = view.rows();
        self.report(system, layout)
    }

    fn resource_area(layout: Layout) -> Rect
    {
        Rect::new(279, 0, layout.width - 16, layout.height - 34)
    }

    fn sync_preview(&mut self) -> ToolboxResult<()>
    {
        let view = self.resource_view.control().info()?;
        self.preview_top = view.top;
        self.preview_count = view.spec.count;
        self.preview_rows = view.rows();
        Ok(())
    }
}

struct HostExport<'a>
{
    system:   System<'a>,
    token:    u64,
    sequence: u32,
    length:   u64,
    hash:     u64,
    buffer:   [u8; 512],
    used:     usize
}
impl HostExport<'_>
{
    fn flush(&mut self) -> fmt::Result
    {
        if self.used == 0
        {
            return Ok(());
        }
        let mut line = Text::<1152>::new();
        write!(line, "RESEDIT_JSON_CHUNK {} {} ", self.token, self.sequence)?;
        for &byte in &self.buffer[..self.used]
        {
            write!(line, "{byte:02x}")?;
        }
        line.write_char('\n')?;
        self.system.log(line.as_str()).map_err(|_| fmt::Error)?;
        self.sequence += 1;
        self.used = 0;
        Ok(())
    }
}
impl Write for HostExport<'_>
{
    fn write_str(&mut self, value: &str) -> fmt::Result
    {
        for &byte in value.as_bytes()
        {
            self.buffer[self.used] = byte;
            self.used += 1;
            self.length += 1;
            self.hash = (self.hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
            if self.used == self.buffer.len()
            {
                self.flush()?;
            }
        }
        Ok(())
    }
}
fn export(system: &System<'_>, browser: &mut Browser<'_>, window: WindowId) -> ToolboxResult<()>
{
    let Some(file) = &browser.file
    else
    {
        return Ok(());
    };
    let Some(catalog) = &browser.catalog
    else
    {
        return Ok(());
    };
    browser.export_number = browser.export_number.checked_add(1).ok_or(Error::BadArgument)?;
    let token = (u64::from(window.0) << 32) | u64::from(browser.export_number);
    let mut begin = Text::<128>::new();
    let _ = writeln!(begin, "RESEDIT_JSON_BEGIN {token}");
    system.log(begin.as_str())?;
    system.quickdraw().cursors().set_standard(StandardCursor::Watch)?;
    let mut output = HostExport { system: *system,
                                  token,
                                  sequence: 0,
                                  length: 0,
                                  hash: 0xcbf29ce484222325,
                                  buffer: [0; 512],
                                  used: 0 };
    let result = document::export(system, file, catalog, &mut output);
    let flushed = output.flush();
    system.quickdraw().cursors().init()?;
    match result
    {
        Ok(()) => (),
        Err(document::ExportError::Service(error)) => return Err(error),
        Err(document::ExportError::Output) => return Err(Error::Io)
    }
    flushed.map_err(|_| Error::Io)?;
    let mut end = Text::<128>::new();
    let _ = writeln!(end, "RESEDIT_JSON_END {token} {} {:016x}", output.length, output.hash);
    system.log(end.as_str())?;
    browser.status = 11;
    Ok(())
}
fn open_dialog<'a>(system: &System<'a>, browser: &mut Browser<'a>, window: WindowId)
                   -> ToolboxResult<()>
{
    let location = standard_file::Location { volume:    browser.volume,
                                             directory: browser.directory };
    if let Some(choice) =
        system.standard_file().standard_get_file(location, standard_file::TypeFilter::All)?
    {
        browser.volume = choice.entry.volume;
        browser.directory = choice.entry.parent;
        browser.open(system, window, choice.entry)?;
    }
    Ok(())
}
struct Count(usize);
impl Write for Count
{
    fn write_str(&mut self, s: &str) -> fmt::Result
    {
        self.0 = self.0.checked_add(s.len()).ok_or(fmt::Error)?;
        Ok(())
    }
}
struct NativeExport<'a>
{
    writer: files::DataWriter<'a>,
    error:  Option<Error>,
    bytes:  [u8; 4096],
    used:   usize
}
impl NativeExport<'_>
{
    fn flush(&mut self) -> ToolboxResult<()>
    {
        self.writer.write_all(&self.bytes[..self.used])?;
        self.used = 0;
        Ok(())
    }
}
impl Write for NativeExport<'_>
{
    fn write_str(&mut self, s: &str) -> fmt::Result
    {
        let mut bytes = s.as_bytes();
        while !bytes.is_empty()
        {
            let n = bytes.len().min(self.bytes.len() - self.used);
            self.bytes[self.used..self.used + n].copy_from_slice(&bytes[..n]);
            self.used += n;
            bytes = &bytes[n..];
            if self.used == self.bytes.len()
               && let Err(e) = self.flush()
            {
                self.error = Some(e);
                return Err(fmt::Error);
            }
        }
        Ok(())
    }
}
fn save(system: &System<'_>, browser: &mut Browser<'_>, save_as: bool) -> ToolboxResult<()>
{
    let Some(catalog) = browser.catalog
    else
    {
        return Ok(());
    };
    let file = browser.file.as_ref().ok_or(Error::NotFound)?;
    let mut name = Text::<128>::new();
    for c in catalog.name().chars().take(26)
    {
        let _ = write!(name, "{c}");
    }
    let _ = write!(name, ".json");
    let prior = browser.saved;
    let choice = if !save_as && prior.is_some()
    {
        prior
    }
    else
    {
        let location = prior.map(|s| s.location())
                            .unwrap_or(standard_file::Location { volume:    browser.volume,
                                                                 directory: browser.directory });
        let default_name = prior.as_ref().map(|s| s.name()).unwrap_or(name.as_str());
        system.standard_file().save_as(location, ui(system, 18, &mut [0; 128])?, default_name)?
    };
    let Some(mut choice) = choice
    else
    {
        return Ok(());
    };
    let mut count = Count(0);
    document::export(system, file, &catalog, &mut count).map_err(|e| {
                                                            match e
                                                            {
                                                                document::ExportError::Service(e) =>
                                                                {
                                                                    e
                                                                }
                                                                document::ExportError::Output =>
                                                                {
                                                                    Error::NoMemory
                                                                }
                                                            }
                                                        })?;
    let spec = files::DataFileSpec { volume:    choice.entry.volume,
                                     parent:    choice.entry.parent,
                                     name:      choice.name(),
                                     replacing: choice.replacing.then_some(choice.entry.id),
                                     file_type: *b"TEXT",
                                     creator:   *b"ttxt",
                                     modified:  system.now()? };
    let mut output = NativeExport { writer: system.files().create_data(spec, count.0)?,
                                    error:  None,
                                    bytes:  [0; 4096],
                                    used:   0 };
    system.quickdraw().cursors().set_standard(StandardCursor::Watch)?;
    let result = document::export(system, file, &catalog, &mut output);
    system.quickdraw().cursors().init()?;
    result.map_err(|e| {
              match e
              {
                  document::ExportError::Service(e) => e,
                  document::ExportError::Output => output.error.unwrap_or(Error::Io)
              }
          })?;
    output.flush()?;
    choice.entry = output.writer.finish()?;
    choice.replacing = true;
    browser.saved = Some(choice);
    browser.status = 11;
    let mut log = Text::<256>::new();
    let _ = writeln!(log,
                     "RESEDIT_SAVED volume={} cnid={} name={} bytes={}",
                     choice.entry.volume,
                     choice.entry.id,
                     choice.name(),
                     count.0);
    system.log(log.as_str())?;
    Ok(())
}
fn run<'a>(system: &System<'a>) -> ToolboxResult<()>
{
    system.menus().clear()?;
    system.menus().install_menu_resource(resources::APPLICATION, 128)?;
    let (bounds, procedure) = window_layout(system)?;
    let mut title_buffer = [0; 128];
    let title = system.resources().application().string(128, &mut title_buffer)?;
    let mut owner = system.windows()
                          .create_owned(windows::WindowSpec::new(bounds, title).procedure(procedure)
                                                                               .minimum(600, 400))?;
    let window = owner.id();
    let mut browser = Browser::new(system, window)?;
    browser.draw(system, window)?;
    system.log("RESEDIT_READY\n")?;
    loop
    {
        let event = system.events().wait_event(mac_toolbox::events::EventMask::ALL, 30)?;
        if system.windows().info(window).is_err_and(|error| error == Error::NotFound)
        {
            return Ok(());
        }
        let command = system.menus().selection(&event)?.map(|s| s.command.to_be_bytes());
        if command.is_some_and(|value| value == *b"quit" || value == *b"clos")
        {
            owner.close()?;
            return Ok(());
        }
        let mut redraw = matches!(event, Event::Update { .. })
                         && event.window().map_or(0, |window| window.0) == window.0;
        if command == Some(*b"open")
        {
            if open_dialog(system, &mut browser, window).is_err()
            {
                browser.status = 12;
            }
            redraw = true;
        }
        if command == Some(*b"save") || command == Some(*b"svas") || command == Some(*b"expr")
        {
            if let Err(error) = save(system, &mut browser, command == Some(*b"svas"))
            {
                let mut log = Text::<128>::new();
                let _ = writeln!(log, "RESEDIT_SAVE_ERROR status={}", error.status());
                system.log(log.as_str())?;
                browser.status = 13;
            }
            redraw = true;
        }
        if command == Some(*b"host")
        {
            if export(system, &mut browser, window).is_err()
            {
                browser.status = 13;
            }
            redraw = true;
        }
        if matches!(event, Event::KeyDown { .. })
           && !event.input().modifiers.contains(mac_toolbox::events::Modifiers::COMMAND)
        {
            let bounds = system.windows().info(window)?.bounds;
            let layout = Layout { width:  bounds.right - bounds.left,
                                  height: bounds.bottom - bounds.top };
            if let (Some(file), Some(item)) = (browser.file.as_ref(), browser.selected)
            {
                browser.resource_view.key(system,
                                           window,
                                           file,
                                           item,
                                           Browser::resource_area(layout),
                                           event.key().map_or(0, |key| key.code()))?;
                browser.sync_preview()?;
                browser.report(system, layout)?;
            }
            system.log("RESEDIT_DRAW_READY\n")?;
            continue;
        }
        if matches!(event, Event::MouseDown { .. })
           && event.window().map_or(0, |window| window.0) == window.0
        {
            let bounds = system.windows().info(window)?.bounds;
            let layout = Layout { width:  bounds.right - bounds.left,
                                  height: bounds.bottom - bounds.top };
            let point = Point { h: event.input().at.h - bounds.left,
                                v: event.input().at.v - bounds.top };
            if Browser::resource_area(layout).contains(point)
            {
                let changed = if let (Some(file), Some(item)) =
                    (browser.file.as_ref(), browser.selected)
                {
                    browser.resource_view.mouse(system,
                                                 window,
                                                 file,
                                                 item,
                                                 Browser::resource_area(layout),
                                                 point)?
                }
                else
                {
                    false
                };
                browser.sync_preview()?;
                if changed
                {
                    browser.draw(system, window)?;
                }
                else
                {
                    browser.report(system, layout)?;
                    system.log("RESEDIT_DRAW_READY\n")?;
                }
                continue;
            }
            if let Some(pane) =
                [Pane::Types, Pane::Resources].into_iter()
                                              .find(|p| layout.scrollbar(*p).contains(point))
            {
                browser.track_scrollbar(system, window, layout, pane, point)?;
                continue;
            }
            else if let Some(row) = layout.row_at(Pane::Types, point)
            {
                let index = browser.type_top + row;
                if index < browser.type_count
                {
                    browser.type_index = index;
                    browser.resource_index = 0;
                    browser.resource_top = 0;
                    browser.preview_top = 0;
                }
            }
            else if let Some(row) = layout.row_at(Pane::Resources, point)
            {
                let index = browser.resource_top + row;
                if index < browser.resource_count
                {
                    browser.resource_index = index;
                    browser.preview_top = 0;
                }
            }
            redraw = true;
        }
        if redraw
        {
            browser.draw(system, window)?;
        }
    }
}
/// # Safety
/// The loader supplies initialized segments and a live service table.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> Status
{
    if let Err(status) = unsafe { mac_toolbox::panic::install(services) }
    {
        return status;
    }
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(system) => system,
        Err(status) => return status.status()
    };
    match run(&system)
    {
        Ok(()) => OK,
        Err(error) =>
        {
            let _ = system.log("RESEDIT_ERROR\n");
            error.status()
        }
    }
}

mac_toolbox::panic_handler!();
