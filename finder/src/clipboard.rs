//! Read-only native clipboard viewer. Layout and text come from Finder resources;
//! all windows, drawing, fonts, controls and Scrap operations use the Toolbox SDK.
use core::fmt::{self, Write};

use mac_toolbox::{controls::{SCROLL_DOWN, SCROLL_PAGE_DOWN, SCROLL_PAGE_UP, SCROLL_THUMB,
                             SCROLL_UP},
                  quickdraw::fonts::FontSelection,
                  scrap::{FlavorInfo, ScrapInfo, TEXT},
                  *};
const HEADER: i16 = 20;
const BAR: i16 = 15;
const FONT: FontSelection = FontSelection { family: 3,
                                            size:   9,
                                            face:   0 };
const LINE_HEIGHT: i16 = 12;
const TITLE: u32 = 22;
const CONTENTS: u32 = 23;
const NONE: u32 = 24;
const TEXT_LABEL: u32 = 25;
const UNSUPPORTED: u32 = 26;
const OVERSIZED: u32 = 27;
const TEXT_CAPACITY: usize = 2048;
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Mode
{
    #[default]
    Empty,
    Text,
    Binary,
    Oversized
}
#[derive(Clone, Copy)]
struct Tracking
{
    horizontal: bool,
    bounds:     Rect,
    initial:    i16,
    maximum:    i16,
    anchor:     Point
}
pub struct Viewer
{
    pub window: u32,
    snapshot:   Option<ScrapInfo>,
    first:      FlavorInfo,
    mode:       Mode,
    text:       [u8; TEXT_CAPACITY],
    length:     usize,
    scroll:     Point,
    extent:     Point,
    tracking:   Option<Tracking>
}
impl Default for Viewer
{
    fn default() -> Self
    {
        Self { window:   0,
               snapshot: None,
               first:    FlavorInfo::default(),
               mode:     Mode::Empty,
               text:     [0; TEXT_CAPACITY],
               length:   0,
               scroll:   Point::default(),
               extent:   Point::default(),
               tracking: None }
    }
}
fn error(error: mac_toolbox::Error) -> Status
{
    error.0
}
fn vertical(width: i16, height: i16) -> Rect
{
    Rect::new(width - BAR, HEADER - 1, width + 1, height - BAR + 1)
}
fn horizontal(width: i16, height: i16) -> Rect
{
    Rect::new(-1, height - BAR, width - BAR + 1, height + 1)
}
/// This Finder-owned WIND descriptor is layout metadata, not a GetNewWindow
/// implementation. Its title is empty: Rust obtains the actual UTF-8 title from
/// STR# through Resource Manager instead of decoding Pascal text in an app.
fn window_layout(system: &System<'_>) -> Result<(Rect, i16), Status>
{
    let resource = system.resources()
                         .application()
                         .get(resources::ResourceKey { kind: *b"WIND",
                                                       id:   2002 })
                         .map_err(error)?;
    let mut data = [0u8; 20];
    if resource.size() != data.len() || resource.read(0, &mut data).map_err(error)? != data.len()
    {
        return Err(BAD_ARGUMENT);
    }
    resource.release().map_err(error)?;
    let word = |at| i16::from_be_bytes([data[at], data[at + 1]]);
    let bounds = Rect::new(word(2), word(0), word(6), word(4));
    let procedure = word(8);
    if bounds.left < 0
       || bounds.top < 20
       || bounds.right > 640
       || bounds.bottom > 480
       || bounds.right - bounds.left < 160
       || bounds.bottom - bounds.top < 80
       || procedure != 8
       || data[10..] != [1, 0, 1, 0, 0, 0, 0, 0, 0, 0]
    {
        return Err(BAD_ARGUMENT);
    }
    Ok((bounds, procedure))
}
impl Viewer
{
    pub fn open(&mut self, system: &System<'_>) -> Result<(), Status>
    {
        if self.window == 0 || system.window(self.window).is_err()
        {
            let mut bytes = [0; 768];
            let title = crate::ui::text(system, TITLE, &mut bytes)?;
            let (bounds, procedure) = window_layout(system)?;
            self.window = system.new_window(bounds, title, (160, 80), procedure)?;
            self.scroll = Point::default();
            self.tracking = None;
            self.snapshot = None;
        }
        system.select_window(self.window)?;
        self.draw(system)
    }

    fn current_text(&self) -> Result<&str, Status>
    {
        core::str::from_utf8(&self.text[..self.length]).map_err(|_| BAD_ARGUMENT)
    }

    fn read(&mut self, system: &System<'_>) -> Result<(), Status>
    {
        let info = system.scrap().info().map_err(error)?;
        if self.snapshot == Some(info)
        {
            return Ok(());
        }
        self.mode = Mode::Empty;
        self.length = 0;
        self.extent = Point::default();
        self.scroll = Point::default();
        self.tracking = None;
        if info.flavors != 0
        {
            self.first = system.scrap().flavor_at(0).map_err(error)?;
            if self.first.kind == TEXT
            {
                if self.first.length as usize > self.text.len()
                {
                    self.mode = Mode::Oversized;
                }
                else
                {
                    self.length = system.scrap().text(&mut self.text).map_err(error)?.len();
                    self.mode = Mode::Text;
                    let mut extent = Point::default();
                    for line in Lines::new(self.current_text()?)
                    {
                        extent.h = extent.h.max(system.quickdraw()
                                                      .fonts()
                                                      .text_width(FONT, line)
                                                      .map_err(error)?);
                        extent.v = extent.v.saturating_add(LINE_HEIGHT);
                    }
                    self.extent = extent;
                }
            }
            else
            {
                self.mode = Mode::Binary;
            }
        }
        self.snapshot = Some(info);
        Ok(())
    }

    /// Detect both clear generations and puts: puts change size/flavor count but
    /// intentionally leave the original-style clear generation unchanged.
    pub fn refresh(&mut self, system: &System<'_>) -> Result<(), Status>
    {
        if self.window == 0
        {
            return Ok(());
        }
        match system.window(self.window)
        {
            Ok(_) =>
            {}
            Err(NOT_FOUND) =>
            {
                self.window = 0;
                self.tracking = None;
                return Ok(());
            }
            Err(status) => return Err(status)
        }
        if self.snapshot != Some(system.scrap().info().map_err(error)?)
        {
            self.draw(system)?;
            system.log("FINDER_CLIPBOARD_CHANGED\n")?;
        }
        Ok(())
    }

    fn limits(&self, width: i16, height: i16) -> Point
    {
        Point { h: (self.extent.h + 10 - width + BAR).max(0),
                v: (self.extent.v + 6 - height + HEADER + BAR).max(0) }
    }

    pub fn draw(&mut self, system: &System<'_>) -> Result<(), Status>
    {
        self.read(system)?;
        let window = system.window(self.window)?;
        let width = window.bounds.right - window.bounds.left;
        let height = window.bounds.bottom - window.bounds.top;
        let limits = self.limits(width, height);
        self.scroll.h = self.scroll.h.clamp(0, limits.h);
        self.scroll.v = self.scroll.v.clamp(0, limits.v);
        system.windows()
              .set_grow_icon_clip(WindowId(self.window),
                                  Some(Rect::new(width - BAR, height - BAR, width, height)))
              .map_err(error)?;
        system.begin_update(self.window)?;
        let result: Result<(), Status> = (|| {
            let port = system.quickdraw().graphics(WindowId(self.window));
            port.reset().map_err(error)?;
            port.colors(0xffffff, 0xffffff).map_err(error)?;
            port.paint_rect(Rect::new(0, 0, width, height)).map_err(error)?;
            port.colors(0, 0xffffff).map_err(error)?;
            let mut prefix = [0; 768];
            let prefix = crate::ui::text(system, CONTENTS, &mut prefix)?;
            // Original header uses separate child views, each inset by1px.
            // The value starts1px beyond the prefix advance.
            let label_x = 6 + port.text(FONT,
                                        Point { h: 5,
                                                v: 14 },
                                        prefix,
                                        0)
                                  .map_err(error)?;
            let mut bytes = [0; 768];
            let mut kind = TypeName::default();
            let label = match self.mode
            {
                Mode::Empty => crate::ui::text(system, NONE, &mut bytes)?,
                Mode::Text | Mode::Oversized => crate::ui::text(system, TEXT_LABEL, &mut bytes)?,
                Mode::Binary =>
                {
                    kind.set(self.first.kind)?;
                    kind.as_str()
                }
            };
            port.text(FONT,
                      Point { h: label_x,
                              v: 14 },
                      label,
                      0)
                .map_err(error)?;
            port.paint_rect(Rect::new(0, 17, width, 18)).map_err(error)?;
            port.paint_rect(Rect::new(0, 19, width, 20)).map_err(error)?;
            port.clip_rect(Rect::new(0, HEADER, width - BAR, height - BAR)).map_err(error)?;
            match self.mode
            {
                Mode::Text =>
                {
                    for (index, line) in Lines::new(self.current_text()?).enumerate()
                    {
                        let baseline = 33 + index as i16 * LINE_HEIGHT - self.scroll.v;
                        if baseline >= HEADER && baseline - 9 < height - BAR
                        {
                            port.text(FONT,
                                      Point { h: 5 - self.scroll.h,
                                              v: baseline },
                                      line,
                                      0)
                                .map_err(error)?;
                        }
                    }
                }
                Mode::Binary | Mode::Oversized =>
                {
                    let body = crate::ui::text(system,
                                               if self.mode == Mode::Binary
                                               {
                                                   UNSUPPORTED
                                               }
                                               else
                                               {
                                                   OVERSIZED
                                               },
                                               &mut bytes)?;
                    port.text(FONT,
                              Point { h: 5,
                                      v: 33 },
                              body,
                              0)
                        .map_err(error)?;
                }
                Mode::Empty =>
                {}
            }
            port.clip_rect(Rect::new(0, 0, width, height)).map_err(error)?;
            let active = system.window(0)?.id == self.window;
            system.controls()
                  .draw_scrollbar(WindowId(self.window),
                                  vertical(width, height),
                                  self.scroll.v,
                                  limits.v,
                                  active)
                  .map_err(error)?;
            system.controls()
                  .draw_scrollbar(WindowId(self.window),
                                  horizontal(width, height),
                                  self.scroll.h,
                                  limits.h,
                                  active)
                  .map_err(error)?;
            Ok(())
        })();
        system.end_update(self.window)?;
        result?;
        system.log("FINDER_CLIPBOARD_READY\n")
    }

    /// Read-only document input. Loaded CDEF1 owns all scrollbar hit/drag math.
    pub fn event(&mut self, system: &System<'_>, event: Event) -> Result<bool, Status>
    {
        if self.window == 0
        {
            return Ok(false);
        }
        if event.kind == EventKind::Update && event.message == self.window
        {
            self.draw(system)?;
            return Ok(true);
        }
        let window = system.window(self.window)?;
        let width = window.bounds.right - window.bounds.left;
        let height = window.bounds.bottom - window.bounds.top;
        let point = Point { h: event.where_.h - window.bounds.left,
                            v: event.where_.v - window.bounds.top };
        let limits = self.limits(width, height);
        if let Some(track) = self.tracking
           && matches!(event.kind, EventKind::Null | EventKind::MouseUp)
        {
            let value = system.controls()
                              .scrollbar_track_value(WindowId(self.window),
                                                     track.bounds,
                                                     track.initial,
                                                     track.maximum,
                                                     track.anchor,
                                                     point)
                              .map_err(error)?;
            let previous = if track.horizontal { self.scroll.h } else { self.scroll.v };
            if track.horizontal
            {
                self.scroll.h = value;
            }
            else
            {
                self.scroll.v = value;
            }
            if event.kind == EventKind::MouseUp
            {
                self.tracking = None;
            }
            if value != previous
            {
                self.draw(system)?;
            }
            return Ok(true);
        }
        if event.kind == EventKind::MouseDown && event.message == self.window
        {
            let horizontal_bar = point.v >= height - BAR;
            let bounds =
                if horizontal_bar { horizontal(width, height) } else { vertical(width, height) };
            let maximum = if horizontal_bar { limits.h } else { limits.v };
            let value = if horizontal_bar { self.scroll.h } else { self.scroll.v };
            let part =
                system.controls()
                      .scrollbar_hit(WindowId(self.window), bounds, value, maximum, true, point)
                      .map_err(error)?;
            let page = if horizontal_bar { width - BAR } else { height - HEADER - BAR };
            let delta = match part
            {
                SCROLL_UP => -LINE_HEIGHT,
                SCROLL_DOWN => LINE_HEIGHT,
                SCROLL_PAGE_UP => -page,
                SCROLL_PAGE_DOWN => page,
                _ => 0
            };
            let value = value.saturating_add(delta).clamp(0, maximum);
            if horizontal_bar
            {
                self.scroll.h = value;
            }
            else
            {
                self.scroll.v = value;
            }
            if part == SCROLL_THUMB
            {
                self.tracking = Some(Tracking { horizontal: horizontal_bar,
                                                bounds,
                                                initial: value,
                                                maximum,
                                                anchor: point });
            }
            self.draw(system)?;
            return Ok(true);
        }
        if matches!(event.kind, EventKind::KeyDown | EventKind::AutoKey)
           && system.window(0)?.id == self.window
        {
            let delta = match (event.message >> 8) as u8
            {
                0x74 => -(height - HEADER - BAR),
                0x79 => height - HEADER - BAR,
                _ => 0
            };
            if delta != 0
            {
                self.scroll.v = self.scroll.v.saturating_add(delta).clamp(0, limits.v);
                self.draw(system)?;
            }
            return Ok(true);
        }
        Ok(false)
    }
}
#[derive(Default)]
struct TypeName
{
    bytes:  [u8; 16],
    length: usize
}
impl Write for TypeName
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        let end = self.length + text.len();
        if end > self.bytes.len()
        {
            return Err(fmt::Error);
        }
        self.bytes[self.length..end].copy_from_slice(text.as_bytes());
        self.length = end;
        Ok(())
    }
}
impl TypeName
{
    fn set(&mut self, kind: [u8; 4]) -> Result<(), Status>
    {
        if kind.iter().all(|byte| (0x20..=0x7e).contains(byte))
        {
            self.write_str(core::str::from_utf8(&kind).map_err(|_| BAD_ARGUMENT)?)
                .map_err(|_| BAD_ARGUMENT)
        }
        else
        {
            write!(self, "0x{:08X}", u32::from_be_bytes(kind)).map_err(|_| BAD_ARGUMENT)
        }
    }

    fn as_str(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.length]).unwrap_or("")
    }
}
struct Lines<'a>
{
    remaining: Option<&'a str>
}
impl<'a> Lines<'a>
{
    fn new(text: &'a str) -> Self
    {
        Self { remaining: Some(text) }
    }
}
impl<'a> Iterator for Lines<'a>
{
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item>
    {
        let text = self.remaining.take()?;
        if let Some(index) = text.find(['\r', '\n'])
        {
            let after = &text[index..];
            let skip = if after.starts_with("\r\n") { 2 } else { 1 };
            self.remaining = Some(&after[skip..]);
            Some(&text[..index])
        }
        else
        {
            Some(text)
        }
    }
}
