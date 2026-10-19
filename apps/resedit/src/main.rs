//! ResEdit read-only resource browser with lossless native JSON export.
#![no_std]
#![no_main]
use core::fmt::{self, Write};

use mac_toolbox::{events::{CMD_KEY, EVERY_EVENT},
                  files::{CatalogInfo, ROOT_ID},
                  quickdraw::{cursors::StandardCursor, fonts::FontSelection},
                  resources::ResourceFile,
                  *};
mod code;
mod document;
mod preview;
mod view;
use document::Text;
use view::{Layout, Pane, Scroll};
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
    preview_top:    u32,
    type_count:     u32,
    resource_count: u32,
    preview_count:  u32,
    code_key:       Option<ResourceKey>,
    code_index:     Option<code::Index>,
    hex_mode:       bool,
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
                  preview_top: 0,
                  type_count: 0,
                  resource_count: 0,
                  preview_count: 0,
                  code_key: None,
                  code_index: None,
                  hex_mode: false,
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
        self.code_key = None;
        self.code_index = None;
        self.hex_mode = false;
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

    fn draw(&mut self, system: &System<'_>, window: WindowId) -> ToolboxResult<()>
    {
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
        system.log("RESEDIT_DRAW_READY\n").map_err(Error)
    }

    fn scroll(&self, layout: Layout, pane: Pane) -> Scroll
    {
        let (top, count) = match pane
        {
            Pane::Types => (self.type_top, self.type_count),
            Pane::Resources => (self.resource_top, self.resource_count),
            Pane::Preview => (self.preview_top, self.preview_count)
        };
        Scroll::new(top, layout.rows(pane), count)
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

    fn track_scrollbar(&mut self, system: &System<'_>, window: WindowId, layout: Layout,
                       pane: Pane, anchor: Point)
                       -> ToolboxResult<()>
    {
        let origin = system.windows().info(window)?.bounds;
        let controls = system.controls();
        let initial = self.scroll(layout, pane);
        let mut state = initial.state(layout.scrollbar(pane));
        let part = controls.scrollbar_hit(window,
                                          state.bounds,
                                          state.value,
                                          state.maximum,
                                          state.active != 0,
                                          anchor)?;
        if !matches!(part,
                     controls::SCROLL_UP
                     | controls::SCROLL_DOWN
                     | controls::SCROLL_PAGE_UP
                     | controls::SCROLL_PAGE_DOWN
                     | controls::SCROLL_THUMB)
        {
            return Ok(());
        }
        let mut next_repeat = 0;
        let mut first = true;
        let mut last = None;
        loop
        {
            let input = system.events().input_state()?;
            let down = input.modifiers & events::BTN_STATE == 0;
            let point = Point { h: input.mouse.h - origin.left,
                                v: input.mouse.v - origin.top };
            if part == controls::SCROLL_THUMB
            {
                state.value = controls.scrollbar_track_value(window,
                                                             state.bounds,
                                                             initial.state(state.bounds).value,
                                                             state.maximum,
                                                             anchor,
                                                             point)?;
            }
            else
            {
                let current = self.scroll(layout, pane);
                state = current.state(state.bounds);
                let hot = controls.scrollbar_hit(window, state.bounds, state.value,
                                                 state.maximum, true, point)?
                          == part;
                if first || down && hot && input.uptime_ticks >= next_repeat
                {
                    let top = current.step(part);
                    if top != current.top
                    {
                        self.set_top(pane, top);
                        self.draw(system, window)?;
                        state = self.scroll(layout, pane).state(state.bounds);
                    }
                    next_repeat = input.uptime_ticks + if first { 20 } else { 6 };
                    first = false;
                }
                state.highlighted = if down && hot { part } else { 0 };
            }
            if part == controls::SCROLL_THUMB
            {
                state.highlighted = if down { part } else { 0 };
            }
            if last != Some((state.value, state.highlighted))
            {
                controls.draw_scrollbar_state(window, state)?;
                last = Some((state.value, state.highlighted));
            }
            if !down
            {
                if part == controls::SCROLL_THUMB
                   && state.value != initial.state(state.bounds).value
                {
                    self.set_top(pane, initial.row_for_value(state.value));
                    self.draw(system, window)?;
                }
                return Ok(());
            }
            system.events().wait_event(0, 1)?;
        }
    }

    fn draw_resources(&mut self, system: &System<'_>, window: WindowId, width: i16, height: i16)
                      -> ToolboxResult<()>
    {
        let layout = Layout { width,
                              height };
        let port = system.quickdraw().graphics(window);
        label(system, window, 10, 18, 2)?;
        label(system, window, 120, 18, 3)?;
        label(system, window, 282, 18, 5)?;
        for pane in [Pane::Types, Pane::Resources]
        {
            port.frame_rect(layout.bounds(pane))?;
        }
        let Some(file) = &self.file
        else
        {
            for pane in [Pane::Types, Pane::Resources, Pane::Preview]
            {
                system.controls().draw_scrollbar_state(window,Scroll::new(0,layout.rows(pane),0).state(layout.scrollbar(pane)))?;
            }
            return Ok(());
        };
        self.type_count = file.type_count()?;
        let kind = if self.type_count == 0 { *b"    " } else { file.type_at(self.type_index)? };
        self.resource_count = if self.type_count == 0 { 0 } else { file.count(kind)? };
        self.type_top = self.scroll(layout, Pane::Types).top;
        self.resource_top = self.scroll(layout, Pane::Resources).top;
        for row in 0..layout.rows(Pane::Types)
        {
            let index = self.type_top + row;
            if index >= self.type_count
            {
                break;
            }
            let value = file.type_at(index)?;
            let y = layout.baseline(Pane::Types, row);
            let body = layout.body(Pane::Types);
            if index == self.type_index
            {
                port.paint_rect(Rect::new(body.left + 1, y - 12, body.right, y + 3))?;
            }
            let mut line = Text::<64>::new();
            line.roman(&value);
            let _ = write!(line, " ({})", file.count(value)?);
            port.clip_rect(Rect::new(8, y - 12, body.right - 1, y + 3))?;
            text(system,
                 window,
                 10,
                 y,
                 line.as_str(),
                 if index == self.type_index { 0xffffff } else { 0 })?;
            port.reset()?;
        }
        for row in 0..layout.rows(Pane::Resources)
        {
            let index = self.resource_top + row;
            if index >= self.resource_count
            {
                break;
            }
            let item = file.at(kind, index)?;
            let y = layout.baseline(Pane::Resources, row);
            let body = layout.body(Pane::Resources);
            if index == self.resource_index
            {
                port.paint_rect(Rect::new(body.left + 1, y - 12, body.right, y + 3))?;
            }
            let mut line = Text::<1024>::new();
            let _ = write!(line, "{}  {}", item.info.key.id, item.info.name());
            port.clip_rect(Rect::new(117, y - 12, body.right - 1, y + 3))?;
            text(system,
                 window,
                 119,
                 y,
                 line.as_str(),
                 if index == self.resource_index { 0xffffff } else { 0 })?;
            port.reset()?;
        }
        if self.resource_count != 0
        {
            let item = file.at(kind, self.resource_index)?;
            port.clip_rect(Rect::new(279, 24, width - 16, height - 34))?;
            let mut meta = Text::<128>::new();
            meta.roman(&kind);
            let _ = write!(meta, "  ID {}  {} bytes", item.info.key.id, item.info.length);
            text(system, window, 283, 37, meta.as_str(), 0)?;
            let mut meta = Text::<128>::new();
            let _ = write!(meta, "Attributes: 0x{:02x}", item.info.attributes);
            text(system, window, 283, 52, meta.as_str(), 0)?;
            text(system,
                 window,
                 283,
                 67,
                 if item.has_name { item.info.name() } else { "(unnamed)" },
                 0)?;
            let native = matches!(&kind, b"RV64" | b"CODE");
            if native && !self.hex_mode && self.code_key != Some(item.info.key)
            {
                self.code_key = Some(item.info.key);
                system.quickdraw().cursors().set_standard(StandardCursor::Watch)?;
                self.code_index = code::Index::build(item.info.length, |at, out| {
                                      file.read(item.info.key, at, out)
                                  }).ok();
                system.quickdraw().cursors().init()?;
            }
            let disassembly = native && !self.hex_mode && self.code_index.is_some();
            if kind == *b"RLOC" && !self.hex_mode
            {
                self.preview_count = code::relocation_count(item.info.length, |at, out| {
                                         file.read(item.info.key, at, out)
                                     }).unwrap_or(1);
                self.preview_top = self.scroll(layout, Pane::Preview).top;
                port.clip_rect(layout.body(Pane::Preview))?;
                let mut row = 0;
                let mut error = None;
                let mut emit = |line: &str| {
                    if let Err(e) = port.text(HEX_FONT,
                                              Point { h: 283,
                                                      v: layout.baseline(Pane::Preview, row) },
                                              line,
                                              0)
                    {
                        error = Some(e);
                    }
                    row += 1;
                };
                if code::relocations(item.info.length,
                                     self.preview_top,
                                     layout.rows(Pane::Preview),
                                     |at, out| file.read(item.info.key, at, out),
                                     &mut emit).is_err()
                {
                    emit("Malformed RLOC; raw bytes export intact.");
                }
                if let Some(e) = error
                {
                    return Err(e);
                }
            }
            else if disassembly
            {
                let index = self.code_index.as_ref().unwrap();
                self.preview_count = index.rows.saturating_add(code::HEADER_ROWS);
                let scroll =
                    Scroll::new(self.preview_top, layout.rows(Pane::Preview), self.preview_count);
                self.preview_top = scroll.top;
                port.clip_rect(layout.body(Pane::Preview))?;
                let mut draw_error = None;
                let mut draw_line = |row: u32, line: &str| {
                    if row >= scroll.top
                       && row - scroll.top < scroll.rows
                       && let Err(e) = port.text(HEX_FONT,
                                                 Point { h: 283,
                                                         v: layout.baseline(Pane::Preview,
                                                                            row - scroll.top) },
                                                 line,
                                                 0)
                    {
                        draw_error = Some(e);
                    }
                };
                let mut row = 0;
                code::loader(file, index.entry, |line| {
                    draw_line(row, line);
                    row += 1;
                });
                let first = scroll.top.saturating_sub(code::HEADER_ROWS);
                row = code::HEADER_ROWS + first;
                let visible_header = code::HEADER_ROWS.saturating_sub(scroll.top).min(scroll.rows);
                index.lines(first,
                            scroll.rows - visible_header,
                            |at, out| file.read(item.info.key, at, out),
                            |line| {
                                draw_line(row, line);
                                row += 1;
                            })?;
                if let Some(e) = draw_error
                {
                    return Err(e);
                }
            }
            else
            {
                let mut data = [0; 32768];
                let structured = !self.hex_mode
                                 && matches!(&kind,
                                             b"MENU" | b"STR " | b"STR#" | b"vers" | b"NABI")
                                 && item.info.length <= data.len();
                let icon = !self.hex_mode
                           && matches!(&kind,
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
                let size;
                let offset;
                if structured || icon
                {
                    offset = 0;
                    size = file.read(item.info.key, 0, &mut data)?;
                    let mut lines = 0;
                    if structured
                       && preview::structured(kind, &data[..size], |_| lines += 1).is_err()
                    {
                        lines += 1;
                    }
                    self.preview_count = lines;
                }
                else
                {
                    self.preview_count = item.info.length.div_ceil(8).min(u32::MAX as usize) as u32;
                    self.preview_top = self.scroll(layout, Pane::Preview).top;
                    offset = self.preview_top as usize * 8;
                    let capacity = (layout.rows(Pane::Preview) as usize * 8).min(data.len());
                    size = file.read(item.info.key, offset, &mut data[..capacity])?;
                }
                self.preview_top = self.scroll(layout, Pane::Preview).top;
                let mut line_number = 0u32;
                let mut draw_error = None;
                port.clip_rect(layout.body(Pane::Preview))?;
                let mut emit = |line: &str| {
                    let visible = if structured
                    {
                        line_number >= self.preview_top
                        && line_number - self.preview_top < layout.rows(Pane::Preview)
                    }
                    else
                    {
                        line_number < layout.rows(Pane::Preview)
                    };
                    if visible
                    {
                        let row =
                            if structured { line_number - self.preview_top } else { line_number };
                        let font = if !structured && !icon { HEX_FONT } else { FONT };
                        if let Err(e) = port.text(font,
                                                  Point { h: 283,
                                                          v: layout.baseline(Pane::Preview, row) },
                                                  line,
                                                  0)
                        {
                            draw_error = Some(e);
                        }
                    }
                    line_number += 1;
                };
                if icon
                {
                    if let Err(e) =
                        port.draw_icon(kind, &data[..size], Rect::new(298, 91, 426, 219))
                    {
                        let mut message = Text::<128>::new();
                        let _ = write!(message, "Icon preview unavailable ({})", e.0);
                        emit(message.as_str());
                    }
                }
                else if structured
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
                if let Some(e) = draw_error
                {
                    return Err(e);
                }
            }
            port.reset()?;
            let mut log = Text::<128>::new();
            let _ = writeln!(log,
                             "RESEDIT_RESOURCE kind={} id={} bytes={} top={} mode={}",
                             u32::from_be_bytes(kind),
                             item.info.key.id,
                             item.info.length,
                             self.preview_top,
                             if disassembly { "disassembly" } else { "preview" });
            system.log(log.as_str()).map_err(Error)?;
        }
        else
        {
            self.preview_count = 0;
            self.preview_top = 0;
        }
        port.reset()?;
        for pane in [Pane::Types, Pane::Resources, Pane::Preview]
        {
            system.controls()
                  .draw_scrollbar_state(window,
                                        self.scroll(layout, pane).state(layout.scrollbar(pane)))?;
        }
        let mut log = Text::<256>::new();
        let _ = writeln!(log,
                         "RESEDIT_VIEW types_top={} types_rows={} resources_top={} \
                          resources_rows={} preview_top={} preview_rows={} preview_max={}",
                         self.type_top,
                         layout.rows(Pane::Types),
                         self.resource_top,
                         layout.rows(Pane::Resources),
                         self.preview_top,
                         layout.rows(Pane::Preview),
                         self.scroll(layout, Pane::Preview).maximum());
        system.log(log.as_str()).map_err(Error)?;
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
    system.menus().install_menu_resource(resources::APPLICATION, 129)?;
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
            if open_dialog(system, &mut browser, window).is_err()
            {
                browser.status = 12;
            }
            redraw = true;
        }
        if command == Some(*b"hex ") || command == Some(*b"dasm")
        {
            browser.hex_mode = command == Some(*b"hex ");
            browser.preview_top = 0;
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
            let bounds = system.windows().info(window)?.bounds;
            let layout = Layout { width:  bounds.right - bounds.left,
                                  height: bounds.bottom - bounds.top };
            let scroll = browser.scroll(layout, Pane::Preview);
            browser.preview_top = match (event.message >> 8) as u8
            {
                0x79 => scroll.step(controls::SCROLL_PAGE_DOWN),
                0x7d => scroll.step(controls::SCROLL_DOWN),
                0x74 => scroll.step(controls::SCROLL_PAGE_UP),
                0x7e => scroll.step(controls::SCROLL_UP),
                0x73 => 0,
                0x77 => scroll.maximum(),
                _ => scroll.top
            };
            redraw = true;
        }
        if event.kind == EventKind::MouseDown && event.message == window.0
        {
            let bounds = system.windows().info(window)?.bounds;
            let layout = Layout { width:  bounds.right - bounds.left,
                                  height: bounds.bottom - bounds.top };
            let point = Point { h: event.where_.h - bounds.left,
                                v: event.where_.v - bounds.top };
            if let Some(pane) =
                [Pane::Types, Pane::Resources, Pane::Preview].into_iter().find(|p| {
                                                                             layout.scrollbar(*p)
                                                                                   .contains(point)
                                                                         })
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
