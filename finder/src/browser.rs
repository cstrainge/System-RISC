//! Finder catalog windows. Every volume, entry and fork comes from the File Manager.
use core::fmt::Write;

use mac_toolbox::{System,
                  files::{CatalogInfo, ROOT_ID, VolumeInfo},
                  quickdraw::{fonts::FontSelection,
                              graphics::GraphicsPort,
                              icons::{IconFlags, standard}},
                  *};

use crate::icon_view::{self, Drawing, Layout, State};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Location
{
    pub volume: u32,
    pub folder: u32
}
#[derive(Clone, Copy, Default)]
struct FolderWindow
{
    location: Location,
    window:   u32,
    selected: Option<u32>,
    scroll:   i16,
    scroll_h: i16
}
#[derive(Clone, Copy)]
struct ScrollTrack
{
    window:     u32,
    bounds:     Rect,
    horizontal: bool,
    initial:    i16,
    maximum:    i16,
    anchor:     Point,
    last:       i16
}
#[derive(Default)]
pub struct Browser
{
    windows:           [Option<FolderWindow>; 8],
    desktop_selection: Option<u32>,
    desktop_active:    bool,
    last_click:        Option<(u32, u32, u32, u32)>,
    tracking:          Option<ScrollTrack>,
    pub startup:       u32
}

pub struct Text<const N: usize = 160>
{
    data:   [u8; N],
    length: usize
}
impl<const N: usize> Default for Text<N>
{
    fn default() -> Self
    {
        Self { data:   [0; N],
               length: 0 }
    }
}
impl<const N: usize> Write for Text<N>
{
    fn write_str(&mut self, s: &str) -> core::fmt::Result
    {
        let end = self.length.checked_add(s.len()).ok_or(core::fmt::Error)?;
        if end > N
        {
            return Err(core::fmt::Error);
        }
        self.data[self.length..end].copy_from_slice(s.as_bytes());
        self.length = end;
        Ok(())
    }
}
impl<const N: usize> Text<N>
{
    pub fn as_str(&self) -> &str
    {
        core::str::from_utf8(&self.data[..self.length]).unwrap_or("")
    }
}

pub struct Port<'a, 's>
{
    pub system:   &'s System<'a>,
    pub graphics: GraphicsPort<'a>
}
impl Drawing for Port<'_, '_>
{
    fn fill(&mut self, bounds: Rect, color: u32) -> Result<(), Status>
    {
        self.graphics.colors(color, 0xffffff).map_err(|e| e.0)?;
        self.graphics.paint_rect(bounds).map_err(|e| e.0)
    }

    fn text_width(&mut self, text: &str) -> Result<i16, Status>
    {
        self.system
            .quickdraw()
            .fonts()
            .text_width(FontSelection { family: 3,
                                        size:   9,
                                        face:   0 },
                        text)
            .map_err(|e| e.0)
    }

    fn text(&mut self, baseline: Point, text: &str, color: u32) -> Result<(), Status>
    {
        self.graphics
            .text(FontSelection { family: 3,
                                  size:   9,
                                  face:   0 },
                  baseline,
                  text,
                  color)
            .map(|_| ())
            .map_err(|e| e.0)
    }

    fn icon(&mut self, id: i16, bounds: Rect, state: State) -> Result<(), Status>
    {
        self.system
            .quickdraw()
            .icons()
            .plot(self.graphics.window_id(),
                  id,
                  bounds,
                  IconFlags { selected: state.selected && state.active,
                              open: state.open,
                              ..Default::default() })
            .map_err(|e| e.0)
    }
}
fn error(e: mac_toolbox::Error) -> Status
{
    e.0
}
fn visible(entry: &CatalogInfo) -> bool
{
    entry.finder_flags() & 0x4000 == 0
}
fn volume_icon(volume: &VolumeInfo) -> i16
{
    if volume.icon_kind == *b"ICN#" { volume.icon_id } else { standard::FLOPPY }
}
fn folder_icon(volume: &VolumeInfo, entry: &CatalogInfo) -> i16
{
    let blessed = u32::from_be_bytes(volume.finder_info[..4].try_into().unwrap());
    if entry.id == blessed
    {
        return standard::SYSTEM_FOLDER;
    }
    if entry.parent == blessed
    {
        match entry.name()
        {
            "Fonts" => return standard::FONTS_FOLDER,
            "Extensions" => return standard::EXTENSIONS_FOLDER,
            "Control Panels" => return standard::CONTROL_PANELS_FOLDER,
            "Preferences" => return standard::PREFERENCES_FOLDER,
            "Startup Items" => return standard::STARTUP_FOLDER,
            "Apple Menu Items" => return standard::APPLE_MENU_FOLDER,
            _ =>
            {}
        }
    }
    standard::FOLDER
}
fn entry_icon(system: &System<'_>, volume: &VolumeInfo, entry: &CatalogInfo) -> i16
{
    if entry.is_directory()
    {
        return folder_icon(volume, entry);
    }
    system.quickdraw()
          .icons()
          .for_file(entry.creator, entry.file_type)
          .unwrap_or(if entry.file_type == *b"APPL" || entry.file_type == *b"FNDR"
                     {
                         standard::APPLICATION
                     }
                     else
                     {
                         standard::DOCUMENT
                     })
}
// The 16-pixel controls overlap the content frame and size-box borders by 1px.
fn vertical_scrollbar(width: i16, height: i16) -> Rect
{
    Rect::new(width - icon_view::SCROLLBAR,
              icon_view::INFO_HEIGHT - 1,
              width + 1,
              height - icon_view::SCROLLBAR + 1)
}

fn horizontal_scrollbar(width: i16, height: i16) -> Rect
{
    Rect::new(-1, height - icon_view::SCROLLBAR, width - icon_view::SCROLLBAR + 1, height + 1)
}

fn grow_icon_clip(width: i16, height: i16) -> Rect
{
    Rect::new(width - icon_view::SCROLLBAR, height - icon_view::SCROLLBAR, width, height)
}

fn entry_rect(layout: &mut Layout, entry: &CatalogInfo, width: i16) -> Rect
{
    let automatic = layout.place(width);
    let saved = entry.icon_location();
    if entry.finder_flags() & 0x0100 != 0 && (saved.h != -1 || saved.v != -1)
    {
        // Ordinary catalog items store the cell origin. The 32px icon sits
        // 16px across and 28px down from it (including the information band).
        // Volume-root desktop positions use a different convention.
        // Negative cell origins are valid; only (-1,-1) is unplaced here.
        let left = saved.h.saturating_add(16);
        let top = saved.v.saturating_add(28);
        Rect::new(left, top, left.saturating_add(32), top.saturating_add(32))
    }
    else
    {
        automatic
    }
}
impl Browser
{
    pub fn initialize(&mut self, system: &System<'_>) -> Result<(), Status>
    {
        let files = system.files();
        let count = files.volume_count().map_err(error)?;
        for index in 0..count
        {
            let volume = files.volume_at(index).map_err(error)?;
            if volume.startup != 0
            {
                self.startup = volume.id;
            }
            let mut line = Text::<256>::default();
            let _ = writeln!(&mut line, "FINDER_VOLUME id={} name={}", volume.id, volume.name());
            system.log(line.as_str())?;
        }
        if self.startup == 0 && count > 0
        {
            self.startup = files.volume_at(0).map_err(error)?.id;
        }
        self.draw_desktop(system)?;
        if self.startup != 0
        {
            self.open(system,
                      Location { volume: self.startup,
                                 folder: ROOT_ID })?;
        }
        Ok(())
    }

    fn is_open(&self, location: Location) -> bool
    {
        self.windows.iter().flatten().any(|w| w.location == location)
    }

    pub fn contains(&self, id: u32) -> bool
    {
        self.windows.iter().flatten().any(|w| w.window == id)
    }

    pub fn draw_desktop(&self, system: &System<'_>) -> Result<(), Status>
    {
        let desktop = system.quickdraw().desktop();
        desktop.begin_update().map_err(error)?;
        let result = (|| {
            let mut draw = Port { system,
                                  graphics: desktop.graphics() };
            for index in 0..system.files().volume_count().map_err(error)?
            {
                let volume = system.files().volume_at(index).map_err(error)?;
                icon_view::draw_item(&mut draw,
                                     volume_icon(&volume),
                                     icon_view::volume_bounds(index),
                                     volume.name(),
                                     State { selected: self.desktop_selection == Some(volume.id),
                                             active:   self.desktop_active,
                                             open:     self.is_open(Location { volume: volume.id,
                                                                               folder: ROOT_ID }),
                                             desktop:  true })?;
            }
            icon_view::draw_item(&mut draw,
                                 standard::TRASH,
                                 icon_view::TRASH_BOUNDS,
                                 "Trash",
                                 State { desktop: true,
                                         active: true,
                                         ..Default::default() })?;
            Ok(())
        })();
        if result.is_ok()
        {
            desktop.end_update().map_err(error)?;
        }
        else
        {
            let _ = desktop.cancel_update();
        }
        result
    }

    pub fn open(&mut self, system: &System<'_>, location: Location) -> Result<u32, Status>
    {
        self.desktop_active = false;
        if let Some(existing) = self.windows.iter().flatten().find(|w| w.location == location)
        {
            system.select_window(existing.window)?;
            return Ok(existing.window);
        }
        self.desktop_active = false;
        let Some(slot) = self.windows.iter().position(Option::is_none)
        else
        {
            return Err(NO_MEMORY);
        };
        let folder = system.files().catalog(location.volume, location.folder).map_err(error)?;
        if !folder.is_directory()
        {
            return Err(BAD_ARGUMENT);
        }
        let volume = system.files().volume(location.volume).map_err(error)?;
        let offset = slot as i16 * 16;
        let id = system.new_window(Rect::new(8 + offset, 46 + offset, 412 + offset, 234 + offset),
                                   if location.folder == ROOT_ID
                                   {
                                       volume.name()
                                   }
                                   else
                                   {
                                       folder.name()
                                   },
                                   (180, 100),
                                   8)?;
        self.windows[slot] = Some(FolderWindow { location,
                                                 window: id,
                                                 ..Default::default() });
        self.redraw_all(system)?;
        let mut line = Text::<256>::default();
        let _ = writeln!(&mut line,
                         "FINDER_FOLDER_OPEN volume={} cnid={} window={} name={}",
                         location.volume,
                         location.folder,
                         id,
                         folder.name());
        system.log(line.as_str())?;
        Ok(id)
    }

    pub fn prune(&mut self, system: &System<'_>) -> Result<(), Status>
    {
        let mut changed = false;
        for window in &mut self.windows
        {
            if let Some(w) = window
               && system.window(w.window).is_err()
            {
                let mut line = Text::<256>::default();
                let _ = writeln!(&mut line,
                                 "FINDER_FOLDER_CLOSE volume={} cnid={} window={}",
                                 w.location.volume, w.location.folder, w.window);
                system.log(line.as_str())?;
                *window = None;
                changed = true;
            }
        }
        if changed
        {
            self.redraw_all(system)?;
            system.log("FINDER_CLOSE_COMMITTED\n")?;
        }
        Ok(())
    }

    pub fn redraw_all(&self, system: &System<'_>) -> Result<(), Status>
    {
        self.draw_desktop(system)?;
        for w in self.windows.iter().flatten()
        {
            self.draw(system, w.window)?;
        }
        Ok(())
    }

    pub fn draw(&self, system: &System<'_>, id: u32) -> Result<(), Status>
    {
        let Some(window) = self.windows.iter().flatten().find(|w| w.window == id)
        else
        {
            return Err(NOT_FOUND);
        };
        let bounds = system.window(id)?.bounds;
        let width = bounds.right - bounds.left;
        let height = bounds.bottom - bounds.top;
        let volume = system.files().volume(window.location.volume).map_err(error)?;
        system.windows()
              .set_grow_icon_clip(WindowId(id), Some(grow_icon_clip(width, height)))
              .map_err(error)?;
        system.begin_update(id)?;
        let graphics = system.quickdraw().graphics(WindowId(id));
        let result = (|| {
            graphics.reset().map_err(error)?;
            let mut draw = Port { system,
                                  graphics };
            draw.fill(Rect::new(0, 0, width, height), 0xffffff)?;
            let active = !self.desktop_active && system.window(0).is_ok_and(|front| front.id == id);
            let mut layout = Layout::new(width);
            let mut count = 0u32;
            let mut extent = Point::default();
            for index in 0..65536
            {
                let entry =
                    match system.files()
                                .entry(window.location.volume, window.location.folder, index)
                    {
                        Ok(e) => e,
                        Err(e) if e.0 == NOT_FOUND => break,
                        Err(e) => return Err(e.0)
                    };
                if !visible(&entry)
                {
                    continue;
                }
                count += 1;
                let name_width = draw.text_width(entry.name())?;
                let mut rect = entry_rect(&mut layout, &entry, name_width);
                let caption = icon_view::label_bounds(rect, name_width);
                extent.h = extent.h.max(caption.right);
                extent.v = extent.v.max(caption.bottom);
                rect.offset(-window.scroll_h, -window.scroll);
                if rect.bottom + 11 < icon_view::INFO_HEIGHT
                   || rect.top >= height - icon_view::SCROLLBAR
                {
                    continue;
                }
                graphics.clip_rect(Rect::new(0,
                                             icon_view::INFO_HEIGHT,
                                             width - icon_view::SCROLLBAR,
                                             height - icon_view::SCROLLBAR))
                        .map_err(error)?;
                icon_view::draw_item(&mut draw,
                                     entry_icon(system, &volume, &entry),
                                     rect,
                                     entry.name(),
                                     State { selected: window.selected == Some(entry.id),
                                             active,
                                             open:
                                                 entry.is_directory()
                                                 && self.is_open(Location { volume: window.location
                                                                                          .volume,
                                                                            folder: entry.id }),
                                             desktop: false })?;
            }
            graphics.clip_rect(Rect::new(0, 0, width, height)).map_err(error)?;
            info_band(&mut draw, width, count, &volume)?;
            system.controls()
                  .draw_scrollbar(WindowId(id),
                                  vertical_scrollbar(width, height),
                                  window.scroll,
                                  (extent.v - height + 15).max(0),
                                  active)
                  .map_err(error)?;
            system.controls()
                  .draw_scrollbar(WindowId(id),
                                  horizontal_scrollbar(width, height),
                                  window.scroll_h,
                                  (extent.h - width + 15).max(0),
                                  active)
                  .map_err(error)?;
            Ok(())
        })();
        system.end_update(id)?;
        result?;
        let mut line = Text::<256>::default();
        let _ = writeln!(&mut line, "FINDER_FOLDER_READY window={}", id);
        system.log(line.as_str())
    }

    pub fn click(&mut self, system: &System<'_>, event: Event) -> Result<(), Status>
    {
        self.tracking = None;
        let mut target = None;
        if event.message == 0
        {
            system.select_desktop()?;
            self.desktop_active = true;
            let mut draw = Port { system,
                                  graphics: system.quickdraw().desktop().graphics() };
            for index in 0..system.files().volume_count().map_err(error)?
            {
                let volume = system.files().volume_at(index).map_err(error)?;
                let icon = icon_view::volume_bounds(index);
                let label = icon_view::label_bounds(icon, draw.text_width(volume.name())?);
                if icon.contains(event.where_) || label.contains(event.where_)
                {
                    target = Some((0, volume.id, ROOT_ID));
                    self.desktop_selection = Some(volume.id);
                    break;
                }
            }
            if target.is_none()
            {
                self.desktop_selection = None;
            }
        }
        else if let Some(slot) =
            self.windows.iter().position(|w| w.is_some_and(|w| w.window == event.message))
        {
            let window = self.windows[slot].unwrap();
            let bounds = system.window(window.window)?.bounds;
            let p = Point { h: event.where_.h - bounds.left,
                            v: event.where_.v - bounds.top };
            if p.h >= bounds.right - bounds.left - icon_view::SCROLLBAR
               || p.v >= bounds.bottom - bounds.top - icon_view::SCROLLBAR
            {
                let horizontal = p.v >= bounds.bottom - bounds.top - 15;
                let (max_h, max_v) = self.limits(system, window, bounds)?;
                let value = if horizontal { window.scroll_h } else { window.scroll };
                let maximum = if horizontal { max_h } else { max_v };
                let control_bounds = if horizontal
                {
                    horizontal_scrollbar(bounds.right - bounds.left, bounds.bottom - bounds.top)
                }
                else
                {
                    vertical_scrollbar(bounds.right - bounds.left, bounds.bottom - bounds.top)
                };
                let part = system.controls()
                                 .scrollbar_hit(WindowId(window.window),
                                                control_bounds,
                                                value,
                                                maximum,
                                                true,
                                                p)
                                 .map_err(error)?;
                if part == mac_toolbox::controls::SCROLL_THUMB
                {
                    self.tracking = Some(ScrollTrack { window: window.window,
                                                       bounds: control_bounds,
                                                       horizontal,
                                                       initial: value,
                                                       maximum,
                                                       anchor: p,
                                                       last: value });
                }
                let step = if horizontal { 64 } else { 52 };
                let page = if horizontal
                {
                    bounds.right - bounds.left - 15
                }
                else
                {
                    bounds.bottom - bounds.top - 35
                };
                let delta = match part
                {
                    mac_toolbox::controls::SCROLL_UP => -step,
                    mac_toolbox::controls::SCROLL_DOWN => step,
                    mac_toolbox::controls::SCROLL_PAGE_UP => -page,
                    mac_toolbox::controls::SCROLL_PAGE_DOWN => page,
                    _ => 0
                };
                if let Some(w) = self.windows[slot].as_mut()
                {
                    let position = if horizontal { &mut w.scroll_h } else { &mut w.scroll };
                    *position = position.saturating_add(delta).clamp(0, maximum);
                }
                self.draw(system, window.window)?;
                return Ok(());
            }
            let mut layout = Layout::new(bounds.right - bounds.left);
            let mut draw = Port { system,
                                  graphics: system.quickdraw().graphics(WindowId(window.window)) };
            for index in 0..65536
            {
                let entry =
                    match system.files()
                                .entry(window.location.volume, window.location.folder, index)
                    {
                        Ok(e) => e,
                        Err(e) if e.0 == NOT_FOUND => break,
                        Err(e) => return Err(e.0)
                    };
                if !visible(&entry)
                {
                    continue;
                }
                let width = draw.text_width(entry.name())?;
                let mut icon = entry_rect(&mut layout, &entry, width);
                icon.offset(-window.scroll_h, -window.scroll);
                if p.v >= icon_view::INFO_HEIGHT
                   && (icon.contains(p) || icon_view::label_bounds(icon, width).contains(p))
                {
                    target = Some((window.window, entry.volume, entry.id));
                    break;
                }
            }
            self.windows[slot].as_mut().unwrap().selected = target.map(|(_, _, id)| id);
            self.desktop_selection = None;
            self.desktop_active = false;
        }
        let double = target.is_some_and(|(window, volume, id)| {
                               self.last_click
                                   .is_some_and(|(last_window, last_volume, last_id, time)| {
                                       window == last_window
                                       && volume == last_volume
                                       && id == last_id
                                       && event.when.wrapping_sub(time) < 30
                                   })
                           });
        self.last_click = target.map(|(w, v, id)| (w, v, id, event.when));
        if double
        {
            let (_, volume, folder) = target.unwrap();
            let entry = system.files().catalog(volume, folder).map_err(error)?;
            if entry.is_directory()
            {
                self.open(system,
                          Location { volume,
                                     folder })?;
            }
        }
        self.redraw_all(system)?;
        system.log("FINDER_CLICK_HANDLED\n")
    }

    pub fn activate(&mut self, event: Event)
    {
        if event.modifiers & mac_toolbox::ACTIVE_FLAG != 0
        {
            self.desktop_active = false;
        }
    }

    pub fn track(&mut self, system: &System<'_>, event: Event) -> Result<(), Status>
    {
        let Some(track) = self.tracking
        else
        {
            return Ok(());
        };
        let Ok(window) = system.window(track.window)
        else
        {
            self.tracking = None;
            return Ok(());
        };
        let point = Point { h: event.where_.h - window.bounds.left,
                            v: event.where_.v - window.bounds.top };
        let value = system.controls()
                          .scrollbar_track_value(WindowId(track.window),
                                                 track.bounds,
                                                 track.initial,
                                                 track.maximum,
                                                 track.anchor,
                                                 point)
                          .map_err(error)?;
        if value != track.last
           && let Some(folder) =
               self.windows.iter_mut().flatten().find(|w| w.window == track.window)
        {
            if track.horizontal
            {
                folder.scroll_h = value;
            }
            else
            {
                folder.scroll = value;
            }
            self.tracking.as_mut().unwrap().last = value;
            self.draw(system, track.window)?;
        }
        if event.kind == EventKind::MouseUp
        {
            self.tracking = None;
        }
        Ok(())
    }

    pub fn open_selection(&mut self, system: &System<'_>) -> Result<(), Status>
    {
        if self.desktop_active
           && let Some(volume) = self.desktop_selection
        {
            self.open(system,
                      Location { volume,
                                 folder: ROOT_ID })?;
            return Ok(());
        }
        let front = system.window(0).ok().map(|w| w.id);
        if let Some(w) = self.windows.iter().flatten().find(|w| Some(w.window) == front)
           && let Some(id) = w.selected
        {
            let entry = system.files().catalog(w.location.volume, id).map_err(error)?;
            if entry.is_directory()
            {
                self.open(system,
                          Location { volume: entry.volume,
                                     folder: entry.id })?;
            }
        }
        else if let Some(volume) = self.desktop_selection
        {
            self.open(system,
                      Location { volume,
                                 folder: ROOT_ID })?;
        }
        Ok(())
    }

    pub fn parent(&mut self, system: &System<'_>) -> Result<(), Status>
    {
        if let Ok(front) = system.window(0)
           && let Some(w) = self.windows.iter().flatten().find(|w| w.window == front.id)
           && w.location.folder != ROOT_ID
        {
            let current =
                system.files().catalog(w.location.volume, w.location.folder).map_err(error)?;
            self.open(system,
                      Location { volume: w.location.volume,
                                 folder: current.parent })?;
        }
        Ok(())
    }

    fn limits(&self, system: &System<'_>, window: FolderWindow, bounds: Rect)
              -> Result<(i16, i16), Status>
    {
        let mut layout = Layout::new(bounds.right - bounds.left);
        let mut extent = Point::default();
        let mut draw = Port { system,
                              graphics: system.quickdraw().graphics(WindowId(window.window)) };
        for index in 0..65536
        {
            let entry = match system.files()
                                    .entry(window.location.volume, window.location.folder, index)
            {
                Ok(e) => e,
                Err(e) if e.0 == NOT_FOUND => break,
                Err(e) => return Err(e.0)
            };
            if !visible(&entry)
            {
                continue;
            }
            let width = draw.text_width(entry.name())?;
            let icon = entry_rect(&mut layout, &entry, width);
            let caption = icon_view::label_bounds(icon, width);
            extent.h = extent.h.max(caption.right);
            extent.v = extent.v.max(caption.bottom);
        }
        Ok(((extent.h - (bounds.right - bounds.left - 15)).max(0),
            (extent.v - (bounds.bottom - bounds.top - 15)).max(0)))
    }

    pub fn scroll(&mut self, system: &System<'_>, delta: i16) -> Result<(), Status>
    {
        if let Ok(front) = system.window(0)
           && let Some(slot) =
               self.windows.iter().position(|w| w.is_some_and(|w| w.window == front.id))
        {
            let window = self.windows[slot].unwrap();
            let (_, maximum) = self.limits(system, window, front.bounds)?;
            self.windows[slot].as_mut().unwrap().scroll =
                window.scroll.saturating_add(delta).clamp(0, maximum);
            self.draw(system, window.window)?;
        }
        Ok(())
    }
}
fn size_text(bytes: u64) -> Text<40>
{
    let mut text = Text::default();
    if bytes >= 1024 * 1024
    {
        let tenths = bytes.saturating_mul(10) / (1024 * 1024);
        if tenths.is_multiple_of(10)
        {
            let _ = write!(&mut text, "{} MB", tenths / 10);
        }
        else
        {
            let _ = write!(&mut text, "{}.{} MB", tenths / 10, tenths % 10);
        }
    }
    else
    {
        let _ = write!(&mut text, "{}K", bytes / 1024);
    }
    text
}
fn info_band(draw: &mut impl Drawing, width: i16, count: u32, volume: &VolumeInfo)
             -> Result<(), Status>
{
    draw.fill(Rect::new(0, 0, width, 20), 0xffffff)?;
    let mut text = Text::<64>::default();
    let _ = write!(&mut text, "{} {}", count, if count == 1 { "item" } else { "items" });
    draw.text(Point { h: 8,
                      v: 12 },
              text.as_str(),
              0)?;
    let mut used = Text::<64>::default();
    let _ = write!(&mut used,
                   "{} in disk",
                   size_text(volume.total_bytes.saturating_sub(volume.free_bytes)).as_str());
    let w = draw.text_width(used.as_str())?;
    draw.text(Point { h: (width - w) / 2,
                      v: 12 },
              used.as_str(),
              0)?;
    let mut free = Text::<64>::default();
    let _ = write!(&mut free, "{} available", size_text(volume.free_bytes).as_str());
    let w = draw.text_width(free.as_str())?;
    draw.text(Point { h: width - 5 - w,
                      v: 12 },
              free.as_str(),
              0)?;
    draw.fill(Rect::new(0, 17, width, 18), 0)?;
    draw.fill(Rect::new(0, 19, width, 20), 0)
}
