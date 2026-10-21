//! Finder catalog windows. Every volume, entry and fork comes from the File Manager.
use core::fmt::Write;

use mac_toolbox::{System,
                  files::{CatalogInfo, ROOT_ID, VolumeInfo},
                  quickdraw::{fonts::FontSelection,
                              graphics::GraphicsPort,
                              icons::{IconFlags, standard}},
                  *};

use crate::icon_view::{self, Drawing, Icon, Layout, State};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Location
{
    pub volume: VolumeId,
    pub folder: Cnid
}
#[derive(Clone, Copy, Default)]
struct FolderWindow
{
    location:   Location,
    window:     WindowId,
    selected:   Option<Location>,
    scroll:     i16,
    scroll_h:   i16,
    scrollbars: [controls::ControlId; 2]
}
#[derive(Clone, Copy)]
struct IconDrag
{
    entry:  Location,
    anchor: Point
}
#[derive(Default)]
pub struct Browser
{
    windows:           [Option<FolderWindow>; 8],
    desktop_selection: Option<VolumeId>,
    desktop_active:    bool,
    last_click:        Option<(WindowId, VolumeId, Cnid, u32)>,
    trash_selected:    bool,
    drag:              Option<IconDrag>,
    trash_hover:       bool,
    pub startup:       VolumeId
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
    fn fill(&mut self, bounds: Rect, color: u32) -> Result<(), Error>
    {
        self.graphics.colors(color, 0xffffff)?;
        self.graphics.paint_rect(bounds)
    }

    fn text_width(&mut self, text: &str) -> Result<i16, Error>
    {
        self.system.quickdraw().fonts().text_width(FontSelection { family: 3,
                                                                   size:   9,
                                                                   face:   0 },
                                                   text)
    }

    fn text(&mut self, baseline: Point, text: &str, color: u32) -> Result<(), Error>
    {
        self.graphics
            .text(FontSelection { family: 3,
                                  size:   9,
                                  face:   0 },
                  baseline,
                  text,
                  color)
            .map(|_| ())
    }

    fn icon(&mut self, icon: Icon, bounds: Rect, state: State) -> Result<(), Error>
    {
        let icons = self.system.quickdraw().icons();
        let flags = IconFlags { selected: state.selected && state.active,
                                open: state.open,
                                ..Default::default() };
        let id = match icon
        {
            Icon::Resource(id) => id,
            Icon::Application { volume,
                                cnid,
                                fallback } =>
            {
                match icons.plot_file(self.graphics.window_id(), volume, cnid, bounds, flags)
                {
                    Ok(()) => return Ok(()),
                    Err(Error::NotFound | Error::BadArgument | Error::NotSupported) => fallback,
                    Err(e) => return Err(e)
                }
            }
        };
        icons.plot(self.graphics.window_id(), id, bounds, flags)
    }
}
fn visible(entry: &CatalogInfo) -> bool
{
    !entry.finder_flags().contains(files::FinderFlags::INVISIBLE)
    && !(entry.parent == ROOT_ID && entry.is_directory() && entry.name() == "Trash")
}
fn volume_icon(volume: &VolumeInfo) -> i16
{
    if volume.icon_kind == TypeCode::new(*b"ICN#") { volume.icon_id } else { standard::FLOPPY }
}
fn folder_icon(volume: &VolumeInfo, entry: &CatalogInfo) -> i16
{
    let blessed = volume.blessed_folder;
    if entry.id == blessed
    {
        return standard::SYSTEM_FOLDER;
    }
    if entry.parent == blessed
    {
        match entry.name()
        {
            "Drivers" => return standard::DRIVERS_FOLDER,
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
pub(crate) fn entry_icon(system: &System<'_>, volume: &VolumeInfo, entry: &CatalogInfo) -> Icon
{
    if entry.id == ROOT_ID
    {
        return volume_icon(volume).into();
    }
    if entry.is_directory()
    {
        return folder_icon(volume, entry).into();
    }
    // Original System generic stationery family: stacked paper with the
    // lower-right fold. App-specific document/pad compositing is separate.
    if entry.is_stationery()
    {
        return (-3985).into();
    }
    let fallback = system.quickdraw()
                         .icons()
                         .for_file(entry.creator, entry.file_type)
                         .unwrap_or(if entry.file_type == TypeCode::new(*b"APPL")
                                       || entry.file_type == TypeCode::new(*b"FNDR")
                                    {
                                        standard::APPLICATION
                                    }
                                    else
                                    {
                                        standard::DOCUMENT
                                    });
    if entry.file_type == TypeCode::new(*b"APPL")
    {
        Icon::Application { volume: volume.id,
                            cnid: entry.id,
                            fallback }
    }
    else
    {
        fallback.into()
    }
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
    if entry.finder_flags().contains(files::FinderFlags::INITIALIZED)
       && (saved.h != -1 || saved.v != -1)
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
// Trash is a Finder view aggregating real per-volume File Manager directories.
fn folder_entry(system: &System<'_>, location: Location, mut index: u32)
                -> Result<CatalogInfo, Error>
{
    if location.volume != VolumeId::NONE
    {
        return system.files().entry(location.volume, location.folder, index);
    }
    for volume in 0..system.files().volume_count()?
    {
        let volume = system.files().volume_at(volume)?;
        let trash = match system.files().trash_directory(volume.id, false)
        {
            Ok(trash) => trash,
            Err(Error::NotFound) => continue,
            Err(error) => return Err(error)
        };
        let child_count = trash.child_count.ok_or(Error::BadAbi)?;
        if index < child_count
        {
            return system.files().entry(volume.id, trash.id, index);
        }
        index -= child_count;
    }
    Err(Error::NotFound)
}
impl Browser
{
    pub fn initialize(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        let files = system.files();
        let count = files.volume_count()?;
        for index in 0..count
        {
            let volume = files.volume_at(index)?;
            if volume.startup
            {
                self.startup = volume.id;
            }
            let mut line = Text::<256>::default();
            let _ = writeln!(&mut line, "FINDER_VOLUME id={} name={}", volume.id, volume.name());
            system.log(line.as_str())?;
        }
        if self.startup == VolumeId::NONE && count > 0
        {
            self.startup = files.volume_at(0)?.id;
        }
        self.draw_desktop(system)?;
        if self.startup != VolumeId::NONE
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

    pub fn contains(&self, id: WindowId) -> bool
    {
        self.windows.iter().flatten().any(|w| w.window == id)
    }

    pub fn draw_desktop(&self, system: &System<'_>) -> Result<(), Error>
    {
        let desktop = system.quickdraw().desktop();
        desktop.begin_update()?;
        let result = (|| {
            let mut draw = Port { system,
                                  graphics: desktop.graphics() };
            for index in 0..system.files().volume_count()?
            {
                let volume = system.files().volume_at(index)?;
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
            let mut label = [0u8; 768];
            let label = crate::ui::text(system, 0, &mut label)?;
            icon_view::draw_item(&mut draw,
                                 if self.trash_count(system)? > 0
                                 {
                                     standard::FULL_TRASH
                                 }
                                 else
                                 {
                                     standard::TRASH
                                 },
                                 icon_view::TRASH_BOUNDS,
                                 label,
                                 State { desktop:  true,
                                         active:   self.desktop_active || self.trash_hover,
                                         selected: self.trash_selected || self.trash_hover,
                                         open:     self.is_open(Location::default()) })?;
            Ok(())
        })();
        if result.is_ok()
        {
            desktop.end_update()?;
        }
        else
        {
            let _ = desktop.cancel_update();
        }
        result
    }

    pub fn open(&mut self, system: &System<'_>, location: Location) -> Result<WindowId, Error>
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
            return Err(Error::NoMemory);
        };
        let trash = location == Location::default();
        let folder = if trash
        {
            CatalogInfo::default()
        }
        else
        {
            system.files().catalog(location.volume, location.folder)?
        };
        if !trash && !folder.is_directory()
        {
            return Err(Error::BadArgument);
        }
        let volume = system.files().volume(if trash { self.startup } else { location.volume })?;
        let offset = slot as i16 * 16;
        let mut title = [0; 768];
        let trash_title = crate::ui::text(system, 0, &mut title)?;
        let id = system.new_window(Rect::new(8 + offset, 46 + offset, 412 + offset, 234 + offset),
                                   if trash
                                   {
                                       trash_title
                                   }
                                   else if location.folder == ROOT_ID
                                   {
                                       volume.name()
                                   }
                                   else
                                   {
                                       folder.name()
                                   },
                                   (180, 100),
                                   8)?;
        let create = |bounds| {
            system.controls().create_scrollbar(id,
                                               controls::ScrollbarState { bounds,
                                                                          ..Default::default() },
                                               1,
                                               1)
        };
        let bars = (|| {
            Ok::<_, Error>([create(vertical_scrollbar(404, 188))?,
                            create(horizontal_scrollbar(404, 188))?])
        })();
        let bars = match bars
        {
            Ok(bars) => bars,
            Err(e) =>
            {
                let _ = system.close_window(id);
                return Err(e);
            }
        };
        self.windows[slot] = Some(FolderWindow { location,
                                                 window: id,
                                                 scrollbars: bars.map(|bar| bar.into_id()),
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

    pub fn prune(&mut self, system: &System<'_>) -> Result<(), Error>
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
            if system.window(WindowId(0)).is_err()
            {
                self.desktop_active = true;
            }
            self.redraw_all(system)?;
            system.log("FINDER_CLOSE_COMMITTED\n")?;
        }
        Ok(())
    }

    pub fn redraw_all(&self, system: &System<'_>) -> Result<(), Error>
    {
        self.draw_desktop(system)?;
        for w in self.windows.iter().flatten()
        {
            self.draw(system, w.window)?;
        }
        Ok(())
    }

    pub fn draw(&self, system: &System<'_>, id: WindowId) -> Result<(), Error>
    {
        let Some(window) = self.windows.iter().flatten().find(|w| w.window == id)
        else
        {
            return Err(Error::NotFound);
        };
        if window.location.volume != VolumeId::NONE && window.location.folder != ROOT_ID
        {
            let entry = system.files().catalog(window.location.volume, window.location.folder)?;
            system.windows().set_title(id, entry.name())?;
        }
        let bounds = system.window(id)?.bounds;
        let width = bounds.right - bounds.left;
        let height = bounds.bottom - bounds.top;
        let volume = system.files().volume(if window.location == Location::default()
                                            {
                                                self.startup
                                            }
                                            else
                                            {
                                                window.location.volume
                                            })?;
        system.windows().set_grow_icon_clip(id, Some(grow_icon_clip(width, height)))?;
        system.begin_update(id)?;
        let graphics = system.quickdraw().graphics(id);
        let result = (|| {
            graphics.reset()?;
            let mut draw = Port { system,
                                  graphics };
            draw.fill(Rect::new(0, 0, width, height), 0xffffff)?;
            let active = !self.desktop_active
                         && system.window(WindowId(0)).is_ok_and(|front| front.id == id);
            let mut layout = Layout::new(width);
            let mut count = 0u32;
            let mut extent = Point::default();
            for index in 0..65536
            {
                let entry = match folder_entry(system, window.location, index)
                {
                    Ok(e) => e,
                    Err(Error::NotFound) => break,
                    Err(e) => return Err(e)
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
                                             height - icon_view::SCROLLBAR))?;
                let icon = entry_icon(system, &system.files().volume(entry.volume)?, &entry);
                // The browser owns the open/running mark. Other consumers,
                // including Get Info, draw the same icon with their own state.
                let open = if entry.is_directory()
                {
                    self.is_open(Location { volume: entry.volume,
                                            folder: entry.id })
                }
                else
                {
                    matches!(icon, Icon::Application { .. })
                    && system.processes().is_running(entry.volume, entry.id)?
                };
                icon_view::draw_item(&mut draw,
                                     icon,
                                     rect,
                                     entry.name(),
                                     State { selected: window.selected
                                                       == Some(Location { volume: entry.volume,
                                                                          folder: entry.id }),
                                             active,
                                             open,
                                             desktop: false })?;
            }
            graphics.clip_rect(Rect::new(0, 0, width, height))?;
            info_band(system,
                      &mut draw,
                      width,
                      count,
                      if window.location == Location::default() { None } else { Some(&volume) })?;
            for (index, bounds, value, maximum, step, page) in [(0,
                                                                 vertical_scrollbar(width, height),
                                                                 window.scroll,
                                                                 (extent.v - height + 15).max(0),
                                                                 52,
                                                                 (height - 35).max(1)),
                                                                (1,
                                                                 horizontal_scrollbar(width,
                                                                                      height),
                                                                 window.scroll_h,
                                                                 (extent.h - width + 15).max(0),
                                                                 64,
                                                                 (width - 15).max(1))]
            {
                let control = system.controls().control(window.scrollbars[index]);
                control.update_scrollbar(controls::ScrollbarState { bounds,
                                                                    value: value.min(maximum),
                                                                    maximum,
                                                                    active: u32::from(active),
                                                                    ..Default::default() },
                                         step,
                                         page)?;
            }
            Ok(())
        })();
        system.end_update(id)?;
        result?;
        let mut line = Text::<256>::default();
        let _ = writeln!(&mut line, "FINDER_FOLDER_READY window={}", id);
        system.log(line.as_str())
    }

    pub fn click(&mut self, system: &System<'_>, event: Event) -> Result<(), Error>
    {
        self.drag = None;
        let mut target: Option<(WindowId, VolumeId, Cnid)> = None;
        if event.window().is_none()
        {
            system.select_desktop()?;
            self.desktop_active = true;
            let mut draw = Port { system,
                                  graphics: system.quickdraw().desktop().graphics() };
            for index in 0..system.files().volume_count()?
            {
                let volume = system.files().volume_at(index)?;
                let icon = icon_view::volume_bounds(index);
                let label = icon_view::label_bounds(icon, draw.text_width(volume.name())?);
                if icon.contains(event.input().at) || label.contains(event.input().at)
                {
                    target = Some((WindowId(0), volume.id, ROOT_ID));
                    self.desktop_selection = Some(volume.id);
                    break;
                }
            }
            self.trash_selected = false;
            if target.is_none()
            {
                self.desktop_selection = None;
                let mut label = [0; 768];
                let label = crate::ui::text(system, 0, &mut label)?;
                let icon = icon_view::TRASH_BOUNDS;
                if icon.contains(event.input().at)
                   || icon_view::label_bounds(icon, draw.text_width(label)?).contains(event.input()
                                                                                           .at)
                {
                    self.trash_selected = true;
                    target = Some((WindowId(0), VolumeId::NONE, Cnid::UNSAVED));
                }
            }
        }
        else if let Some(slot) =
            self.windows
                .iter()
                .position(|w| w.is_some_and(|w| w.window == event.window().unwrap_or_default()))
        {
            let window = self.windows[slot].unwrap();
            let bounds = system.window(window.window)?.bounds;
            let p = Point { h: event.input().at.h - bounds.left,
                            v: event.input().at.v - bounds.top };
            if p.h >= bounds.right - bounds.left - icon_view::SCROLLBAR
               || p.v >= bounds.bottom - bounds.top - icon_view::SCROLLBAR
            {
                for (index, id) in window.scrollbars.into_iter().enumerate()
                {
                    if system.controls().control(id).track(p, |action| {
                                                         if let Some(folder) =
                                                             self.windows[slot].as_mut()
                                                         {
                                                             if index == 1
                                                             {
                                                                 folder.scroll_h =
                                                                     action.scrollbar.value;
                                                             }
                                                             else
                                                             {
                                                                 folder.scroll =
                                                                     action.scrollbar.value;
                                                             }
                                                         }
                                                         self.draw(system, window.window)
                                                     })?
                    {
                        break;
                    }
                }
                return Ok(());
            }
            let mut layout = Layout::new(bounds.right - bounds.left);
            let mut draw = Port { system,
                                  graphics: system.quickdraw().graphics(window.window) };
            for index in 0..65536
            {
                let entry = match folder_entry(system, window.location, index)
                {
                    Ok(e) => e,
                    Err(Error::NotFound) => break,
                    Err(e) => return Err(e)
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
            self.windows[slot].as_mut().unwrap().selected = target.map(|(_, volume, id)| {
                                                                      Location { volume,
                                                                                 folder: id }
                                                                  });
            self.trash_selected = false;
            self.drag = target.map(|(_, volume, folder)| {
                                  IconDrag { entry:  Location { volume,
                                                                folder },
                                             anchor: event.input().at }
                              });
            self.desktop_selection = None;
            self.desktop_active = false;
        }
        let double = target.is_some_and(|(window, volume, id)| {
                               self.last_click
                                   .is_some_and(|(last_window, last_volume, last_id, time)| {
                                       window == last_window
                                       && volume == last_volume
                                       && id == last_id
                                       && event.input().when.wrapping_sub(time) < 30
                                   })
                           });
        self.last_click = target.map(|(w, v, id)| (w, v, id, event.input().when));
        if double
        {
            let (_, volume, folder) = target.unwrap();
            self.drag = None;
            if volume == VolumeId::NONE
            {
                self.open(system, Location::default())?;
                self.redraw_all(system)?;
                return Ok(());
            }
            let entry = system.files().catalog(volume, folder)?;
            if entry.is_directory()
            {
                self.open(system,
                          Location { volume,
                                     folder })?;
            }
            else if entry.file_type == TypeCode::new(*b"APPL")
            {
                system.processes().launch(volume, folder)?;
            }
        }
        self.redraw_all(system)?;
        system.log("FINDER_CLICK_HANDLED\n")
    }

    pub fn activate(&mut self, event: Event)
    {
        if event.input().modifiers.contains(mac_toolbox::events::Modifiers::ACTIVE)
        {
            self.desktop_active = false;
        }
    }

    pub fn track(&mut self, system: &System<'_>, event: Event) -> Result<(), Error>
    {
        if let Some(drag) = self.drag
        {
            let moved = (i32::from(event.input().at.h) - i32::from(drag.anchor.h)).abs() > 4
                        || (i32::from(event.input().at.v) - i32::from(drag.anchor.v)).abs() > 4;
            let hover = moved && icon_view::TRASH_BOUNDS.contains(event.input().at);
            if self.trash_hover != hover
            {
                self.trash_hover = hover;
                self.draw_desktop(system)?;
            }
            if matches!(event, Event::MouseUp { .. })
            {
                self.drag = None;
                self.trash_hover = false;
                if hover
                {
                    self.move_entry_to_trash(system, drag.entry)?;
                }
                else
                {
                    self.draw_desktop(system)?;
                }
            }
        }
        Ok(())
    }

    pub fn open_selection(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        if self.desktop_active && self.trash_selected
        {
            self.open(system, Location::default())?;
            return Ok(());
        }
        if self.desktop_active
           && let Some(volume) = self.desktop_selection
        {
            self.open(system,
                      Location { volume,
                                 folder: ROOT_ID })?;
            return Ok(());
        }
        let front = system.window(WindowId(0)).ok().map(|w| w.id);
        if let Some(w) = self.windows.iter().flatten().find(|w| Some(w.window) == front)
           && let Some(id) = w.selected
        {
            let entry = system.files().catalog(id.volume, id.folder)?;
            if entry.is_directory()
            {
                self.open(system,
                          Location { volume: entry.volume,
                                     folder: entry.id })?;
            }
            else if entry.file_type == TypeCode::new(*b"APPL")
            {
                system.processes().launch(entry.volume, entry.id)?;
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

    pub fn parent(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        if let Ok(front) = system.window(WindowId(0))
           && let Some(w) = self.windows.iter().flatten().find(|w| w.window == front.id)
           && w.location.volume != VolumeId::NONE
           && w.location.folder != ROOT_ID
        {
            let current = system.files().catalog(w.location.volume, w.location.folder)?;
            self.open(system,
                      Location { volume: w.location.volume,
                                 folder: current.parent })?;
        }
        Ok(())
    }

    fn limits(&self, system: &System<'_>, window: FolderWindow, bounds: Rect)
              -> Result<(i16, i16), Error>
    {
        let mut layout = Layout::new(bounds.right - bounds.left);
        let mut extent = Point::default();
        let mut draw = Port { system,
                              graphics: system.quickdraw().graphics(window.window) };
        for index in 0..65536
        {
            let entry = match folder_entry(system, window.location, index)
            {
                Ok(e) => e,
                Err(Error::NotFound) => break,
                Err(e) => return Err(e)
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

    pub fn scroll(&mut self, system: &System<'_>, delta: i16) -> Result<(), Error>
    {
        if let Ok(front) = system.window(WindowId(0))
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
fn info_band(system: &System<'_>, draw: &mut impl Drawing, width: i16, count: u32,
             volume: Option<&VolumeInfo>)
             -> Result<(), Error>
{
    draw.fill(Rect::new(0, 0, width, 20), 0xffffff)?;
    let mut text = Text::<64>::default();
    let mut bytes = [0; 768];
    let item = crate::ui::text(system, if count == 1 { 18 } else { 19 }, &mut bytes)?;
    let _ = write!(&mut text, "{} {}", count, item);
    draw.text(Point { h: 8,
                      v: 12 },
              text.as_str(),
              0)?;
    if let Some(capacity) = volume.and_then(|volume| volume.capacity)
    {
        let mut used = Text::<64>::default();
        let _ = write!(&mut used,
                       "{}{}",
                       size_text(capacity.total_bytes.saturating_sub(capacity.free_bytes)).as_str(),
                       crate::ui::text(system, 20, &mut bytes)?);
        let w = draw.text_width(used.as_str())?;
        draw.text(Point { h: (width - w) / 2,
                          v: 12 },
                  used.as_str(),
                  0)?;
        let mut free = Text::<64>::default();
        let _ = write!(&mut free,
                       "{}{}",
                       size_text(capacity.free_bytes).as_str(),
                       crate::ui::text(system, 21, &mut bytes)?);
        let w = draw.text_width(free.as_str())?;
        draw.text(Point { h: width - 5 - w,
                          v: 12 },
                  free.as_str(),
                  0)?;
    }
    draw.fill(Rect::new(0, 17, width, 18), 0)?;
    draw.fill(Rect::new(0, 19, width, 20), 0)
}

impl Browser
{
    pub fn selection(&self, system: &System<'_>) -> Option<Location>
    {
        if self.desktop_active
        {
            return None;
        }
        let front = system.window(WindowId(0)).ok()?.id;
        self.windows.iter().flatten().find(|w| w.window == front)?.selected
    }

    pub fn info_selection(&self, system: &System<'_>) -> Option<Location>
    {
        if self.desktop_active
        {
            self.desktop_selection.map(|volume| {
                                      Location { volume,
                                                 folder: ROOT_ID }
                                  })
        }
        else
        {
            self.selection(system)
        }
    }

    pub fn current_folder(&self, system: &System<'_>) -> Option<Location>
    {
        if self.desktop_active
        {
            return (self.startup != VolumeId::NONE).then_some(Location { volume: self.startup,
                                                                         folder: ROOT_ID });
        }
        let front = system.window(WindowId(0)).ok()?.id;
        self.windows.iter().flatten().find(|w| w.window == front).map(|w| w.location)
    }

    pub fn can_open(&self, system: &System<'_>) -> bool
    {
        self.desktop_active && (self.trash_selected || self.desktop_selection.is_some())
        || self.selection(system).is_some_and(|s| {
                                     system.files().catalog(s.volume, s.folder).is_ok_and(|e| {
                                                                                   e.is_directory()
                                                                                   || e.file_type
                                                                                      == TypeCode::new(*b"APPL")
                                                                               })
                                 })
    }

    pub fn can_new_folder(&self, system: &System<'_>) -> bool
    {
        self.current_folder(system).is_some_and(|location| {
                                       location.volume != VolumeId::NONE
                                       && system.files()
                                                .volume(location.volume)
                                                .is_ok_and(|v| !v.read_only)
                                   })
    }

    pub fn can_trash(&self, system: &System<'_>) -> bool
    {
        let Some(selection) = self.selection(system)
        else
        {
            return false;
        };
        let Ok(volume) = system.files().volume(selection.volume)
        else
        {
            return false;
        };
        let blessed = volume.blessed_folder;
        let Ok(entry) = system.files().catalog(selection.volume, selection.folder)
        else
        {
            return false;
        };
        !volume.read_only
        && entry.id != ROOT_ID
        && entry.id != blessed
        && !(entry.parent == blessed && ["System", "Finder"].contains(&entry.name()))
        && !system.files()
                  .trash_directory(selection.volume, false)
                  .is_ok_and(|trash| entry.parent == trash.id || entry.id == trash.id)
    }

    pub fn can_put_away(&self, system: &System<'_>) -> bool
    {
        let Some(selection) = self.selection(system)
        else
        {
            return false;
        };
        system.files().catalog(selection.volume,selection.folder).is_ok_and(|e|
            e.put_away_parent()!=Cnid::UNSAVED && system.files().trash_directory(selection.volume,false).is_ok_and(|t|t.id==e.parent)
            && system.files().volume(selection.volume).is_ok_and(|v|!v.read_only))
    }

    pub fn trash_count(&self, system: &System<'_>) -> Result<u32, Error>
    {
        let mut count = 0u32;
        for index in 0..system.files().volume_count()?
        {
            let volume = system.files().volume_at(index)?;
            match system.files().trash_directory(volume.id, false)
            {
                Ok(trash) =>
                {
                    count = count.saturating_add(trash.child_count.ok_or(Error::BadAbi)?)
                }
                Err(Error::NotFound) =>
                {}
                Err(e) => return Err(e)
            }
        }
        Ok(count)
    }

    fn files_changed(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        self.last_click = None;
        self.drag = None;
        self.trash_hover = false;
        for window in self.windows.iter_mut().flatten()
        {
            window.selected = None;
            if window.location.volume != VolumeId::NONE
               && system.files().catalog(window.location.volume, window.location.folder).is_err()
            {
                system.close_window(window.window)?;
            }
        }
        self.prune(system)?;
        self.redraw_all(system)
    }

    pub fn new_folder(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        let location = self.current_folder(system).ok_or(Error::BadArgument)?;
        if location.volume == VolumeId::NONE
        {
            return Err(Error::BadArgument);
        }
        let mut bytes = [0; 768];
        let base = crate::ui::text(system, 1, &mut bytes)?;
        let mut created = None;
        for suffix in 1..1000
        {
            let mut name = Text::<96>::default();
            write!(&mut name, "{}", base).map_err(|_| Error::BadArgument)?;
            if suffix > 1
            {
                write!(&mut name, " {}", suffix).map_err(|_| Error::BadArgument)?;
            }
            match system.files().new_folder(location.volume, location.folder, name.as_str())
            {
                Ok(entry) =>
                {
                    created = Some(entry);
                    break;
                }
                Err(mac_toolbox::Error::AlreadyExists) =>
                {}
                Err(e) => return Err(e)
            }
        }
        let created = created.ok_or(mac_toolbox::Error::AlreadyExists)?;
        self.files_changed(system)?;
        if self.desktop_active
        {
            self.open(system, location)?;
        }
        if let Some(w) = self.windows.iter_mut().flatten().find(|w| w.location == location)
        {
            w.selected = Some(Location { volume: created.volume,
                                         folder: created.id });
        }
        self.redraw_all(system)?;
        let mut log = Text::<160>::default();
        let _ = writeln!(&mut log,
                         "FINDER_NEW_FOLDER volume={} cnid={} parent={} name={}",
                         created.volume,
                         created.id,
                         created.parent,
                         created.name());
        system.log(log.as_str())
    }

    fn move_entry_to_trash(&mut self, system: &System<'_>, entry: Location) -> Result<(), Error>
    {
        let moved = system.files().move_to_trash(entry.volume, entry.folder)?;
        self.files_changed(system)?;
        let mut log = Text::<160>::default();
        let _ = writeln!(&mut log,
                         "FINDER_TRASHED volume={} cnid={} parent={} original={}",
                         moved.volume,
                         moved.id,
                         moved.parent,
                         moved.put_away_parent());
        system.log(log.as_str())
    }

    pub fn trash_selection(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        let entry = self.selection(system).ok_or(Error::NotFound)?;
        self.move_entry_to_trash(system, entry)
    }

    pub fn put_away(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        let entry = self.selection(system).ok_or(Error::NotFound)?;
        let moved = system.files().put_away(entry.volume, entry.folder)?;
        self.files_changed(system)?;
        let mut log = Text::<160>::default();
        let _ = writeln!(&mut log,
                         "FINDER_PUT_AWAY volume={} cnid={} parent={}",
                         moved.volume, moved.id, moved.parent);
        system.log(log.as_str())
    }

    pub fn empty_trash(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        let mut removed = 0;
        let result = (|| -> Result<(), Error> {
            for index in 0..system.files().volume_count()?
            {
                let volume = system.files().volume_at(index)?;
                let trash = match system.files().trash_directory(volume.id, false)
                {
                    Ok(trash) => trash,
                    Err(Error::NotFound) => continue,
                    Err(e) => return Err(e)
                };
                if trash.child_count.ok_or(Error::BadAbi)? > 0
                {
                    removed += system.files().empty_trash(volume.id)?;
                }
            }
            Ok(())
        })();
        // Each volume commits independently. Refresh even if a later volume fails.
        let refresh = self.files_changed(system);
        if let Err(status) = result
        {
            let mut log = Text::<80>::default();
            let _ = writeln!(&mut log,
                             "FINDER_TRASH_PARTIAL removed={} status={}",
                             removed,
                             status.status());
            system.log(log.as_str())?;
            return Err(status);
        }
        refresh?;
        let mut log = Text::<80>::default();
        let _ = writeln!(&mut log, "FINDER_TRASH_EMPTIED removed={}", removed);
        system.log(log.as_str())
    }
}
impl Browser
{
    pub fn trash_usage(&self, system: &System<'_>)
                       -> Result<mac_toolbox::files::TrashUsage, Error>
    {
        let mut total = mac_toolbox::files::TrashUsage { items:           0,
                                                         allocated_bytes: 0 };
        for index in 0..system.files().volume_count()?
        {
            let volume = system.files().volume_at(index)?;
            let usage = system.files().trash_usage(volume.id)?;
            total.items = total.items.checked_add(usage.items).ok_or(Error::BadArgument)?;
            total.allocated_bytes = total.allocated_bytes
                                         .checked_add(usage.allocated_bytes)
                                         .ok_or(Error::BadArgument)?;
        }
        Ok(total)
    }
}
pub fn disk_size_text(bytes: u64) -> Text<40>
{
    size_text(bytes)
}
