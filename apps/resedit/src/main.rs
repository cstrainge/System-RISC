//! ResEdit read-only resource browser with lossless native JSON export.
#![no_std]
#![no_main]
use core::fmt::{self, Write};

use mac_toolbox::{events::{CMD_KEY, EVERY_EVENT},
                  files::{CatalogInfo, ROOT_ID},
                  quickdraw::{cursors::StandardCursor, fonts::FontSelection},
                  resources::{FileResourceInfo, ResourceFile},
                  *};
mod document;
mod preview;
use document::Text;
const FONT: FontSelection = FontSelection { family: 3,
                                            size:   9,
                                            face:   0 };
const ROWS: u32 = 16;
const ROW_TOP: i16 = 70;
const ROW_HEIGHT: i16 = 16;

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
fn frame(system: &System<'_>, window: WindowId, bounds: Rect, index: u32) -> ToolboxResult<()>
{
    system.controls().draw_standard_state(window,
                                          controls::StandardKind::Button,
                                          controls::StandardState::new(bounds,
                                                                       ui(system, index,
                                                                          &mut [0; 256])?))
}
fn button_at(height: i16, point: Point) -> Option<(Rect, u32)>
{
    [(Rect::new(8, 8, 76, 32), 0),
     (Rect::new(84, 8, 198, 32), 1),
     (Rect::new(8, height - 59, 54, height - 36), 8),
     (Rect::new(58, height - 59, 105, height - 36), 7),
     (Rect::new(117, height - 59, 187, height - 36), 8),
     (Rect::new(191, height - 59, 268, height - 36), 7)].into_iter()
                                                        .find(|(bounds, _)| bounds.contains(point))
}
fn track_button(system: &System<'_>, window: WindowId, bounds: Rect, title: u32)
                -> ToolboxResult<bool>
{
    let mut buffer = [0; 256];
    let mut state = controls::StandardState::new(bounds, ui(system, title, &mut buffer)?);
    let origin = system.windows().info(window)?.bounds;
    let mut highlighted = false;
    loop
    {
        let mouse = system.events().mouse()?;
        let inside = bounds.contains(Point { h: mouse.h - origin.left,
                                             v: mouse.v - origin.top });
        let down = system.events().button()?;
        if highlighted != (inside && down)
        {
            highlighted = inside && down;
            state.highlighted = u32::from(highlighted);
            system.controls().draw_standard_state(window, controls::StandardKind::Button, state)?;
            system.events().next_event(0)?;
        }
        if !down
        {
            return Ok(inside);
        }
        system.events().wait_event(0, 1)?;
    }
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
        return Err(Error(BAD_ARGUMENT));
    }
    let word = |at| i16::from_be_bytes([data[at], data[at + 1]]);
    let bounds = Rect::new(word(2), word(0), word(6), word(4));
    if bounds.right.checked_sub(bounds.left).is_none_or(|n| n < 600)
       || bounds.bottom.checked_sub(bounds.top).is_none_or(|n| n < 400)
       || word(8) != 0
    {
        return Err(Error(BAD_ARGUMENT));
    }
    Ok((bounds, word(8)))
}
struct Browser<'a>
{
    catalog:        Option<CatalogInfo>,
    file:           Option<ResourceFile<'a>>,
    type_index:     u32,
    resource_index: u32,
    type_top:       u32,
    resource_top:   u32,
    page:           usize,
    saved:          Option<standard_file::Selection>,
    volume:         u32,
    directory:      u32,
    status:         u32,
    export_number:  u32
}
impl<'a> Browser<'a>
{
    fn new(system: &System<'a>) -> ToolboxResult<Self>
    {
        let volume = system.files().volume_at(0)?.id;
        let directory =
            system.files().lookup(volume, ROOT_ID, "Applications").map(|e| e.id).unwrap_or(ROOT_ID);
        Ok(Self { catalog: None,
                  file: None,
                  type_index: 0,
                  resource_index: 0,
                  type_top: 0,
                  resource_top: 0,
                  page: 0,
                  saved: None,
                  volume,
                  directory,
                  status: 9,
                  export_number: 0 })
    }

    fn open(&mut self, system: &System<'a>, item: CatalogInfo) -> ToolboxResult<()>
    {
        let file = system.resources().open_file(item.volume, item.id)?;
        self.file = Some(file);
        self.catalog = Some(item);
        self.type_index = 0;
        self.resource_index = 0;
        self.type_top = 0;
        self.resource_top = 0;
        self.page = 0;
        self.saved = None;
        self.status = 10;
        let mut log = Text::<256>::new();
        let _ = writeln!(log,
                         "RESEDIT_OPEN volume={} cnid={} name={}",
                         item.volume,
                         item.id,
                         item.name());
        system.log(log.as_str()).map_err(Error)
    }

    fn selected(&self) -> ToolboxResult<Option<FileResourceInfo>>
    {
        let Some(file) = &self.file
        else
        {
            return Ok(None);
        };
        if file.type_count()? == 0
        {
            return Ok(None);
        }
        let kind = file.type_at(self.type_index)?;
        if file.count(kind)? == 0
        {
            return Ok(None);
        }
        file.at(kind, self.resource_index).map(Some)
    }

    fn draw(&self, system: &System<'_>, window: WindowId) -> ToolboxResult<()>
    {
        let bounds = system.windows().info(window)?.bounds;
        let width = bounds.right - bounds.left;
        let height = bounds.bottom - bounds.top;
        system.windows().begin_update(window)?;
        let port = system.quickdraw().graphics(window);
        port.reset()?;
        port.colors(0xffffff, 0xffffff)?;
        port.paint_rect(Rect::new(0, 0, width, height))?;
        port.colors(0, 0xffffff)?;
        frame(system, window, Rect::new(8, 8, 76, 32), 0)?;
        frame(system, window, Rect::new(84, 8, 198, 32), 1)?;
        self.draw_resources(system, window, width, height)?;
        port.clip_rect(Rect::new(4, height - 26, width - 18, height - 3))?;
        label(system, window, 10, height - 10, self.status)?;
        port.reset()?;
        system.windows().end_update(window)?;
        system.log("RESEDIT_DRAW_READY\n").map_err(Error)
    }

    fn draw_resources(&self, system: &System<'_>, window: WindowId, width: i16, height: i16)
                      -> ToolboxResult<()>
    {
        let Some(file) = &self.file
        else
        {
            return Ok(());
        };
        text(system, window, 210, 24, self.catalog.as_ref().map_or("", |e| e.name()), 0)?;
        label(system, window, 10, 53, 2)?;
        label(system, window, 120, 53, 3)?;
        label(system, window, 282, 53, 5)?;
        let port = system.quickdraw().graphics(window);
        port.frame_rect(Rect::new(4, 57, 109, height - 34))?;
        port.frame_rect(Rect::new(113, 57, 272, height - 34))?;
        let count = file.type_count()?;
        let kind = if count == 0 { *b"    " } else { file.type_at(self.type_index)? };
        for row in 0..ROWS
        {
            let index = self.type_top + row;
            if index >= count
            {
                break;
            }
            let value = file.type_at(index)?;
            let y = ROW_TOP + row as i16 * ROW_HEIGHT;
            if index == self.type_index
            {
                port.paint_rect(Rect::new(6, y - 12, 107, y + 3))?;
            }
            let mut line = Text::<64>::new();
            line.roman(&value);
            let _ = write!(line, " ({})", file.count(value)?);
            port.clip_rect(Rect::new(8, y - 12, 106, y + 3))?;
            text(system,
                 window,
                 10,
                 y,
                 line.as_str(),
                 if index == self.type_index { 0xffffff } else { 0 })?;
            port.reset()?;
        }
        for row in 0..ROWS
        {
            let index = self.resource_top + row;
            if index >= file.count(kind)?
            {
                break;
            }
            let item = file.at(kind, index)?;
            let y = ROW_TOP + row as i16 * ROW_HEIGHT;
            if index == self.resource_index
            {
                port.paint_rect(Rect::new(115, y - 12, 270, y + 3))?;
            }
            let mut line = Text::<1024>::new();
            let _ = write!(line, "{}  {}", item.info.key.id, item.info.name());
            port.clip_rect(Rect::new(117, y - 12, 269, y + 3))?;
            text(system,
                 window,
                 119,
                 y,
                 line.as_str(),
                 if index == self.resource_index { 0xffffff } else { 0 })?;
            port.reset()?;
        }
        // Each list has independent paging, so all types and resources remain reachable.
        frame(system, window, Rect::new(8, height - 59, 54, height - 36), 8)?;
        frame(system, window, Rect::new(58, height - 59, 105, height - 36), 7)?;
        frame(system, window, Rect::new(117, height - 59, 187, height - 36), 8)?;
        frame(system, window, Rect::new(191, height - 59, 268, height - 36), 7)?;
        let Some(item) = self.selected()?
        else
        {
            return Ok(());
        };
        port.clip_rect(Rect::new(279, 58, width - 16, height - 34))?;
        let mut meta = Text::<128>::new();
        meta.roman(&item.info.key.kind);
        let _ = write!(meta, "  ID {}  {} bytes", item.info.key.id, item.info.length);
        text(system, window, 283, 72, meta.as_str(), 0)?;
        let mut meta = Text::<128>::new();
        let _ = write!(meta, "Attributes: 0x{:02x}", item.info.attributes);
        text(system, window, 283, 87, meta.as_str(), 0)?;
        text(system,
             window,
             283,
             102,
             if item.has_name { item.info.name() } else { "(unnamed)" },
             0)?;
        let mut data = [0u8; 32768];
        let structured = matches!(&kind, b"MENU" | b"STR " | b"STR#" | b"vers")
                         && item.info.length <= data.len();
        let icon = matches!(&kind,
                            b"ICON"
                            | b"ICN#"
                            | b"ics#"
                            | b"SICN"
                            | b"cicn"
                            | b"icl4"
                            | b"icl8"
                            | b"ics4"
                            | b"ics8")
                   && item.info.length <= data.len();
        let offset = if structured || icon
        {
            0
        }
        else
        {
            self.page.saturating_mul(128).min(item.info.length)
        };
        let limit = if structured || icon { data.len() } else { 128 };
        let size = file.read(item.info.key, offset, &mut data[..limit])?;
        let mut line_number = 0usize;
        let mut draw_error = None;
        let mut emit = |line: &str| {
            let visible = if structured
            {
                line_number >= self.page.saturating_mul(14)
                && line_number < self.page.saturating_mul(14).saturating_add(14)
            }
            else
            {
                line_number < 16
            };
            if visible
            {
                let y = 128
                        + (if structured
                        {
                            line_number - self.page.saturating_mul(14)
                        }
                        else
                        {
                            line_number
                        }) as i16
                          * 14;
                if let Err(error) = text(system, window, 283, y, line, 0)
                {
                    draw_error = Some(error);
                }
            }
            line_number += 1;
        };
        if icon
        {
            if let Err(error) = port.draw_icon(kind, &data[..size], Rect::new(298, 126, 426, 254))
            {
                let mut message = Text::<128>::new();
                let _ = write!(message, "Icon preview unavailable ({})", error.0);
                emit(message.as_str());
            }
        }
        else if structured && item.info.length <= data.len()
        {
            if preview::structured(kind, &data[..size], &mut emit).is_err()
            {
                emit("Malformed resource; raw bytes export intact.");
            }
        }
        else
        {
            preview::hex(&data[..size], offset, &mut emit);
        }
        if let Some(error) = draw_error
        {
            return Err(error);
        }
        port.reset()?;
        label(system, window, 283, height - 44, 17)?;
        let mut log = Text::<128>::new();
        let _ = writeln!(log,
                         "RESEDIT_RESOURCE kind={} id={} bytes={}",
                         u32::from_be_bytes(kind),
                         item.info.key.id,
                         item.info.length);
        system.log(log.as_str()).map_err(Error)
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
    browser.export_number = browser.export_number.checked_add(1).ok_or(Error(BAD_ARGUMENT))?;
    let token = (u64::from(window.0) << 32) | u64::from(browser.export_number);
    let mut begin = Text::<128>::new();
    let _ = writeln!(begin, "RESEDIT_JSON_BEGIN {token}");
    system.log(begin.as_str()).map_err(Error)?;
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
        Err(document::ExportError::Output) => return Err(Error(IO_ERROR))
    }
    flushed.map_err(|_| Error(IO_ERROR))?;
    let mut end = Text::<128>::new();
    let _ = writeln!(end, "RESEDIT_JSON_END {token} {} {:016x}", output.length, output.hash);
    system.log(end.as_str()).map_err(Error)?;
    browser.status = 11;
    Ok(())
}
fn open_dialog<'a>(system: &System<'a>, browser: &mut Browser<'a>) -> ToolboxResult<()>
{
    let location = standard_file::Location { volume:    browser.volume,
                                             directory: browser.directory };
    if let Some(choice) =
        system.standard_file().standard_get_file(location, standard_file::TypeFilter::All)?
    {
        browser.volume = choice.entry.volume;
        browser.directory = choice.entry.parent;
        browser.open(system, choice.entry)?;
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
    let file = browser.file.as_ref().ok_or(Error(NOT_FOUND))?;
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
                                                                    Error(NO_MEMORY)
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
                  document::ExportError::Output => output.error.unwrap_or(Error(IO_ERROR))
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
    system.log(log.as_str()).map_err(Error)?;
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
    let mut browser = Browser::new(system)?;
    browser.draw(system, window)?;
    system.log("RESEDIT_READY\n").map_err(Error)?;
    loop
    {
        let event = system.events().wait_event(EVERY_EVENT, 30)?;
        if system.windows().info(window).is_err_and(|error| error.0 == NOT_FOUND)
        {
            return Ok(());
        }
        let command = system.menus().selection(&event)?.map(|s| s.command.to_be_bytes());
        if command.is_some_and(|value| value == *b"quit" || value == *b"clos")
        {
            owner.close()?;
            return Ok(());
        }
        let mut redraw = event.kind == EventKind::Update && event.message == window.0;
        if command == Some(*b"open")
        {
            if open_dialog(system, &mut browser).is_err()
            {
                browser.status = 12;
            }
            redraw = true;
        }
        if command == Some(*b"save") || command == Some(*b"svas") || command == Some(*b"expr")
        {
            if save(system, &mut browser, command == Some(*b"svas")).is_err()
            {
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
        if matches!(event.kind, EventKind::KeyDown | EventKind::AutoKey)
           && event.modifiers & CMD_KEY == 0
        {
            match (event.message >> 8) as u8
            {
                0x79 | 0x7d => browser.page = browser.page.saturating_add(1),
                0x74 | 0x7e => browser.page = browser.page.saturating_sub(1),
                0x73 => browser.page = 0,
                _ => ()
            };
            redraw = true;
        }
        if event.kind == EventKind::MouseDown && event.message == window.0
        {
            let bounds = system.windows().info(window)?.bounds;
            let x = event.where_.h - bounds.left;
            let y = event.where_.v - bounds.top;
            if let Some((button, title)) = button_at(bounds.bottom - bounds.top,
                                                     Point { h: x,
                                                             v: y })
            {
                if !track_button(system, window, button, title)?
                {
                    browser.draw(system, window)?;
                    continue;
                }
                if y < 36
                {
                    if x < 80
                    {
                        if open_dialog(system, &mut browser).is_err()
                        {
                            browser.status = 12;
                        }
                    }
                    else if save(system, &mut browser, false).is_err()
                    {
                        browser.status = 13;
                    }
                }
                else if let Some(file) = &browser.file
                {
                    match x
                    {
                        8..=54 => browser.type_top = browser.type_top.saturating_sub(ROWS),
                        58..=105 if browser.type_top + ROWS < file.type_count()? =>
                        {
                            browser.type_top += ROWS
                        }
                        117..=187 =>
                        {
                            browser.resource_top = browser.resource_top.saturating_sub(ROWS)
                        }
                        191..=268
                            if file.type_count()? != 0
                               && browser.resource_top + ROWS
                                  < file.count(file.type_at(browser.type_index)?)? =>
                        {
                            browser.resource_top += ROWS
                        }
                        _ => ()
                    }
                }
            }
            else if (ROW_TOP - 12..ROW_TOP + (ROWS as i16 - 1) * ROW_HEIGHT + 4).contains(&y)
            {
                let row = ((y - (ROW_TOP - 12)) / ROW_HEIGHT) as u32;
                if let Some(file) = &browser.file
                {
                    if x < 109
                    {
                        let index = browser.type_top + row;
                        if index < file.type_count()?
                        {
                            browser.type_index = index;
                            browser.resource_index = 0;
                            browser.resource_top = 0;
                            browser.page = 0;
                        }
                    }
                    else if x < 272 && file.type_count()? != 0
                    {
                        let index = browser.resource_top + row;
                        if index < file.count(file.type_at(browser.type_index)?)?
                        {
                            browser.resource_index = index;
                            browser.page = 0;
                        }
                    }
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
        Err(status) => return status
    };
    match run(&system)
    {
        Ok(()) => OK,
        Err(error) =>
        {
            let _ = system.log("RESEDIT_ERROR\n");
            error.0
        }
    }
}

mac_toolbox::panic_handler!();
