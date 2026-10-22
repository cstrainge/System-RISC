//! Finder catalog windows. Every volume, entry and fork comes from the File Manager.
use core::{cell::Cell, fmt::Write};
#[path = "desktop.rs"]
pub(crate) mod desktop;
#[path = "dragging.rs"]
pub(crate) mod dragging;

use mac_toolbox::{System,
                  files::{CatalogInfo, ROOT_ID, VolumeInfo},
                  quickdraw::{fonts::FontSelection,
                              graphics::GraphicsPort,
                              icons::{IconFlags, standard}},
                  *};

use crate::{cache,
            icon_view::{self, Caption, Drawing, Icon, Layout, State},
            selection::{CAPACITY, Selection}};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Location
{
    pub volume: VolumeId,
    pub folder: Cnid
}
#[derive(Clone, Copy, Default)]
struct FolderWindow
{
    location:       Location,
    window:         WindowId,
    selected:       Selection,
    outline:        Option<Rect>,
    scroll:         i16,
    scroll_h:       i16,
    scrollbars:     [controls::ControlId; 2],
    zoom_restore:   Option<(Rect, Point)>,
    pending_clicks: [Option<(mac_toolbox::events::classic::Event, PointerPosition)>; 8]
}
#[derive(Clone, Copy)]
struct IconPress
{
    entry:    Location,
    anchor:   Point,
    collapse: Option<(usize, usize)>
}
#[derive(Clone, Copy)]
struct Marquee
{
    slot:   usize,
    anchor: Point,
    band:   Rect,
    base:   Selection
}
#[derive(Default)]
pub struct Browser
{
    front:            WindowId,
    cache_job:        Option<usize>,
    startup_writable: bool,
    windows:          [Option<FolderWindow>; 8],
    desktop:          desktop::State,
    desktop_active:   bool,
    last_click:       Option<(WindowId, VolumeId, Cnid, u32)>,
    press:            Option<IconPress>,
    marquee:          Option<Marquee>,
    trash_hover:      bool,
    pub startup:      VolumeId,
    trash_cached:     Cell<u32>,
    failures:         [Cell<Option<Error>>; 8]
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

    fn icon(&mut self, icon: Icon, name: &str, bounds: Rect, state: State) -> Result<(), Error>
    {
        let icons = self.system.quickdraw().icons();
        let flags = IconFlags { selected: state.selected && state.active,
                                open: state.open,
                                ..Default::default() };
        let id = match icon
        {
            Icon::Resource(id) => id,
            Icon::Associated { file_type,
                               fallback } =>
            {
                match icons.plot_file_default(self.graphics.window_id(),
                                              file_type,
                                              name,
                                              bounds,
                                              flags)
                {
                    Ok(()) => return Ok(()),
                    Err(Error::NotFound | Error::NotSupported) => fallback,
                    Err(error) => return Err(error)
                }
            }
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
        Icon::Associated { file_type: entry.file_type,
                           fallback }
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

#[allow(dead_code)] // Original-system visual reference renderer shares this helper.
pub(crate) fn entry_rect(layout: &mut Layout, entry: &CatalogInfo, caption: Caption) -> Rect
{
    let automatic = layout.place(caption);
    if let Some(bounds) = icon_view::saved_icon_bounds(entry)
    {
        // Ordinary catalog items store the cell origin. The 32px icon sits
        // 16px across and 28px down from it (including the information band).
        // Volume-root desktop positions use a different convention.
        // Negative cell origins are valid; only (-1,-1) is unplaced here.
        bounds
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
    pub fn marquee_tracking(&self) -> bool
    {
        self.marquee.is_some() || self.desktop.marquee.is_some()
    }

    pub fn can_select_all(&self) -> bool
    {
        if self.desktop_active
        {
            return desktop::count() > 0;
        }
        self.front_slot()
            .is_some_and(|slot| cache::with(slot, |folder| folder.ready && folder.count != 0))
    }

    pub fn pending(&self) -> bool
    {
        self.windows.iter().enumerate().any(|(slot, window)| {
                                           window.is_some()
                                           && self.failures[slot].get().is_none()
                                           && !cache::with(slot, |folder| folder.ready)
                                       })
    }

    fn invalidate_cache(&mut self, slot: usize)
    {
        let window = self.windows[slot].unwrap();
        cache::edit(slot, |folder| {
            if folder.ready
            {
                folder.previous_count = 0;
                for index in window.selected.iter()
                {
                    if index < folder.count
                    {
                        let item = folder.items[index];
                        let output = folder.previous_count;
                        folder.previous[output] = Some(item.location());
                        folder.previous_count += 1;
                    }
                }
            }
            if folder.ready
            {
                folder.ready = false;
                folder.selected_before = window.selected;
                folder.refresh_pending = true;
            }
            else if !folder.refresh_pending
            {
                folder.cursor = None;
                folder.count = 0;
                folder.restored = Selection::EMPTY;
            }
        });
        self.windows[slot].as_mut().unwrap().selected = Selection::EMPTY;
    }

    /// One bounded backend step per event-loop turn. No pointer handler fills or
    /// validates a snapshot. ScanStale discards the unpublished partial result.
    pub fn advance_cache(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        let active = self.cache_job.filter(|&slot| {
                                       self.windows[slot].is_some()
                                       && self.failures[slot].get().is_none()
                                       && !cache::with(slot, |f| f.ready)
                                   });
        let Some(slot) = active.or_else(|| {
                                   self.windows.iter().enumerate().find_map(|(slot, window)| {
                                                                      (window.is_some()
                                                             && self.failures[slot].get().is_none()
                                                             && !cache::with(slot, |folder| {
                                                                 folder.ready
                                                             })).then_some(slot)
                                                                  })
                               })
        else
        {
            self.cache_job = None;
            return Ok(());
        };
        self.cache_job = Some(slot);
        let window = self.windows[slot].unwrap();
        cache::start_refresh(slot);
        let result = (|| {
            if cache::with(slot, |folder| folder.volume.is_none())
            {
                let volume = system.files().volume(if window.location.volume == VolumeId::NONE
                                                    {
                                                        self.startup
                                                    }
                                                    else
                                                    {
                                                        window.location.volume
                                                    })?;
                cache::edit(slot, |folder| folder.volume = Some(volume));
            }
            let entry = if window.location.volume == VolumeId::NONE
            {
                let index = cache::edit(slot, |folder| {
                    let index = folder.next;
                    folder.next += 1;
                    index
                });
                match folder_entry(system, window.location, index)
                {
                    Ok(entry) => Some(entry),
                    Err(Error::NotFound) => None,
                    Err(error) => return Err(error)
                }
            }
            else
            {
                let mut cursor = cache::with(slot, |folder| folder.cursor.unwrap_or_default());
                let step =
                    system.files().scan_directory(window.location.volume, window.location.folder,
                                                  &mut cursor);
                match step
                {
                    Err(Error::ScanStale) =>
                    {
                        cache::edit(slot, |folder| {
                            folder.cursor = None;
                            folder.count = 0;
                            folder.restored = Selection::EMPTY;
                        });
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                    Ok(step) =>
                    {
                        cache::edit(slot, |folder| folder.cursor = Some(cursor));
                        match step
                        {
                            files::ScanStep::Entry { catalog,
                                                     .. } => Some(catalog),
                            files::ScanStep::Pending => return Ok(()),
                            files::ScanStep::Done => None
                        }
                    }
                }
            };
            if let Some(entry) = entry
            {
                if !visible(&entry)
                {
                    return Ok(());
                }
                if cache::with(slot, |folder| folder.count == CAPACITY)
                {
                    return Err(Error::NoMemory);
                }
                let volume = if window.location.volume == VolumeId::NONE
                {
                    system.files().volume(entry.volume)?
                }
                else
                {
                    cache::with(slot, |folder| folder.volume.unwrap())
                };
                let icon = entry_icon(system, &volume, &entry);
                let running = matches!(icon, Icon::Application { .. })
                              && system.processes().is_running(entry.volume, entry.id)?;
                let trash = if let Some(trash) =
                    cache::with(slot, |folder| folder.trash).filter(|_| {
                                                                window.location.volume
                                                                != VolumeId::NONE
                                                            })
                {
                    trash
                }
                else
                {
                    let trash = match system.files().trash_directory(entry.volume, false)
                    {
                        Ok(entry) => Some(entry.id),
                        Err(Error::NotFound | Error::NotSupported) => None,
                        Err(error) => return Err(error)
                    };
                    cache::edit(slot, |folder| folder.trash = Some(trash));
                    trash
                };
                let can_trash = !volume.read_only
                                && entry.id != ROOT_ID
                                && entry.id != volume.blessed_folder
                                && !(entry.parent == volume.blessed_folder
                                     && ["System", "Finder"].contains(&entry.name()))
                                && !trash.is_some_and(|id| entry.id == id || entry.parent == id);
                let can_put_away = !volume.read_only
                                   && entry.put_away_parent() != Cnid::UNSAVED
                                   && trash == Some(entry.parent);
                let previous = cache::prior(slot,
                                            Location { volume: entry.volume,
                                                       folder: entry.id });
                let caption = if let Some(old) = previous.filter(|old| old.entry.name == entry.name)
                {
                    old.caption
                }
                else
                {
                    Caption::measure(entry.name(), |text| {
                        system.quickdraw().fonts().text_width(FontSelection { family: 3,
                                                                              size:   9,
                                                                              face:   0 },
                                                              text)
                    })?
                };
                cache::edit(slot, |folder| {
                    let index = folder.count;
                    let location = Location { volume: entry.volume,
                                              folder: entry.id };
                    if folder.previous[..folder.previous_count].contains(&Some(location))
                    {
                        folder.restored.set(index, true);
                    }
                    folder.items[index] =
                        cache::Item { entry,
                                      icon,
                                      caption,
                                      running,
                                      can_trash,
                                      can_put_away,
                                      icon_bounds:
                                          previous.filter(|old| {
                                                      entry.saved_icon_location().is_none()
                                                      && old.entry.saved_icon_location().is_none()
                                                  })
                                                  .map_or(Rect::ZERO, |old| old.icon_bounds),
                                      bounds: Rect::ZERO };
                    folder.count += 1;
                });
            }
            else
            {
                let bounds = system.window(window.window)?.bounds;
                let restored = cache::edit(slot, |folder| {
                    folder.layout(bounds.right - bounds.left)?;
                    folder.ready = true;
                    Ok::<_, Error>(folder.restored)
                })?;
                self.windows[slot].as_mut().unwrap().selected = restored;
                let mut layout = Text::<200>::default();
                cache::with(slot, |folder| {
                    if folder.count != 0
                    {
                        let first = folder.items[0].icon_bounds;
                        let frame = folder.frame;
                        let _ = writeln!(&mut layout,
                                         "FINDER_FOLDER_LAYOUT window={} first={},{},{},{} \
                                          frame={},{},{},{} count={}",
                                         window.window,
                                         first.left,
                                         first.top,
                                         first.right,
                                         first.bottom,
                                         frame.left,
                                         frame.top,
                                         frame.right,
                                         frame.bottom,
                                         folder.count);
                    }
                });
                if !layout.as_str().is_empty()
                {
                    system.log(layout.as_str())?;
                }
                self.persist_new_positions(system, slot)?;
                self.repaint_snapshot(system, slot)?;
                let pending =
                    core::mem::take(&mut self.windows[slot].as_mut().unwrap().pending_clicks);
                for (event, position) in pending.into_iter().flatten()
                {
                    let event = Event::from(event);
                    if matches!(event, Event::MouseDown { .. })
                    {
                        self.click(system, &event, position)?;
                    }
                    else
                    {
                        self.track(system, &event, position)?;
                    }
                }
                if cache::edit(slot, |folder| core::mem::take(&mut folder.notify_changed))
                {
                    let mut line = Text::<100>::default();
                    let _ = writeln!(&mut line, "FINDER_FOLDER_CHANGED window={}", window.window);
                    system.log(line.as_str())?;
                }
            }
            Ok(())
        })();
        if let Err(error) = result
        {
            if error == Error::NotFound && system.window(window.window).is_err()
            {
                return Ok(());
            }
            self.failures[slot].set(Some(error));
            self.draw_failed(system, window.window, error)?;
            return Err(error);
        }
        Ok(())
    }

    // Keep scan, position-batch and repaint scratch in separate call frames.
    // Combining their CAPACITY-sized arrays can overflow a 256 KiB app stack
    // once resident trap/definition frames are included.
    #[inline(never)]
    fn persist_new_positions(&mut self, system: &System<'_>, slot: usize) -> Result<(), Error>
    {
        let mut rows = [files::IconMove::default(); CAPACITY];
        let (volume, count) = cache::with(slot, |folder| {
            if folder.location.volume == VolumeId::NONE
               || folder.volume.is_none_or(|volume| volume.read_only)
            {
                return (VolumeId::NONE, 0);
            }
            let mut count = 0;
            for index in folder.placements.iter()
            {
                let item = folder.items[index];
                rows[count] =
                    files::IconMove { id:              item.entry.id,
                                      expected_parent: item.entry.parent,
                                      parent:          item.entry.parent,
                                      placement:
                                          files::Placement::At(Point { h: item.icon_bounds
                                                                              .left
                                                                              .saturating_sub(16),
                                                                       v: item.icon_bounds
                                                                              .top
                                                                              .saturating_sub(28) }),
                                      to_trash:        false };
                count += 1;
            }
            (folder.location.volume, count)
        });
        if count == 0
        {
            return Ok(());
        }
        match system.files().move_icons(volume, &rows[..count])
        {
            Ok(_) =>
            {}
            Err(Error::NotSupported) =>
            {
                system.log("FINDER_POSITION_SAVE_UNSUPPORTED\n")?;
                return Ok(());
            }
            Err(Error::ReadOnly) => return Ok(()),
            Err(error) => return Err(error)
        }
        cache::edit(slot, |folder| {
            let placements = folder.placements;
            for index in placements.iter()
            {
                let item = &mut folder.items[index];
                let flags = item.entry.finder_flags().bits() | 0x100;
                item.entry.finder_info[8..10].copy_from_slice(&flags.to_be_bytes());
                item.entry.finder_info[10..12].copy_from_slice(&item.icon_bounds
                                                                    .top
                                                                    .saturating_sub(28)
                                                                    .to_be_bytes());
                item.entry.finder_info[12..14].copy_from_slice(&item.icon_bounds
                                                                    .left
                                                                    .saturating_sub(16)
                                                                    .to_be_bytes());
            }
            folder.placements = Selection::EMPTY;
        });
        Ok(())
    }

    #[inline(never)]
    fn repaint_snapshot(&mut self, system: &System<'_>, slot: usize) -> Result<(), Error>
    {
        let window = self.windows[slot].unwrap();
        let mut damage = [Rect::ZERO; CAPACITY * 2];
        let mut count = 0;
        let mut append = |mut bounds: Rect| {
            bounds.offset(-window.scroll_h, -window.scroll);
            damage[count] = bounds;
            count += 1;
        };
        let details = cache::compare(slot, |folder, previous| {
            let previous = previous?;
            let mut current = [0u64; CAPACITY];
            for (index, item) in folder.items[..folder.count].iter().enumerate()
            {
                current[index] =
                    (u64::from(item.entry.volume.0) << 32) | u64::from(item.entry.id.0);
                if let Some(old_index) = previous.find(item.location())
                {
                    let old = previous.items[old_index];
                    if old.entry != item.entry
                       || old.icon != item.icon
                       || old.running != item.running
                       || old.bounds != item.bounds
                       || previous.selected.contains(old_index) != window.selected.contains(index)
                    {
                        append(old.bounds);
                        append(item.bounds);
                    }
                }
                else
                {
                    append(item.bounds);
                }
            }
            current[..folder.count].sort_unstable();
            for old in &previous.items[..previous.count]
            {
                let key = (u64::from(old.entry.volume.0) << 32) | u64::from(old.entry.id.0);
                if current[..folder.count].binary_search(&key).is_err()
                {
                    append(old.bounds);
                }
            }
            Some((previous.count != folder.count
                  || previous.capacity != folder.volume.and_then(|v| v.capacity),
                  previous.extent != folder.extent))
        });
        let Some((header, bars)) = details
        else
        {
            return self.draw(system, window.window);
        };
        self.change_selection(system, slot, window.selected, &damage[..count])?;
        self.refresh_chrome(system, slot, header, bars, false)?;
        cache::edit(slot, |folder| folder.refreshing = false);
        system.log("FINDER_FOLDER_PATCHED\n")
    }

    fn refresh_chrome(&self, system: &System<'_>, slot: usize, header: bool, bars: bool,
                      full: bool)
                      -> Result<(), Error>
    {
        let window = self.windows[slot].unwrap();
        let (frame, volume, count) = cache::with(slot, |f| (f.frame, f.volume, f.count));
        let width = frame.right - frame.left;
        let height = frame.bottom - frame.top;
        if header
        {
            let graphics = system.quickdraw().graphics(window.window);
            let update =
                graphics.masked_rectangles(&[Rect::new(0, 0, width, icon_view::INFO_HEIGHT)])?;
            graphics.clip_rect(Rect::new(0, 0, width, icon_view::INFO_HEIGHT))?;
            info_band(system,
                      &mut Port { system,
                                  graphics },
                      width,
                      count as u32,
                      volume.as_ref())?;
            update.commit()?;
        }
        if bars
        {
            self.configure_zoom(system, slot)?;
            let active = !self.desktop_active && self.front == window.window;
            let ranges = self.scroll_ranges(slot);
            for (index, bounds, origin, step, page) in
                [(0, vertical_scrollbar(width, height), window.scroll, 52, (height - 35).max(1)),
                 (1, horizontal_scrollbar(width, height), window.scroll_h, 64, (width - 15).max(1))]
            {
                let range = ranges[index];
                let control = system.controls().control(window.scrollbars[index]);
                let configured =
                    control.configure_scrollbar(controls::ScrollbarState { bounds,
                                                                           value:
                                                                               range.value(origin),
                                                                           maximum:
                                                                               range.steps(),
                                                                           active:
                                                                               u32::from(active),
                                                                           ..Default::default() },
                                                step,
                                                page)?;
                if full && !configured.changed
                {
                    control.draw()?;
                }
            }
        }
        Ok(())
    }

    pub fn folder_locations(&self) -> impl Iterator<Item = (WindowId, Location)> + '_
    {
        self.windows
            .iter()
            .flatten()
            .filter(|w| w.location.volume != VolumeId::NONE)
            .map(|w| (w.window, w.location))
    }

    pub fn folder_changed(&mut self, _system: &System<'_>, window: WindowId) -> Result<(), Error>
    {
        let Some(slot) = self.windows.iter().position(|w| w.is_some_and(|w| w.window == window))
        else
        {
            return Ok(());
        };
        self.failures[slot].set(None);
        self.invalidate_cache(slot);
        cache::edit(slot, |folder| folder.notify_changed = true);
        Ok(())
    }

    fn open_entry(&mut self, system: &System<'_>, entry: &CatalogInfo) -> Result<(), Error>
    {
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
        else if let Some(mounted) = system.files().open_item(entry.volume, entry.id)?
        {
            desktop::invalidate();
            self.open(system,
                      Location { volume: mounted.id,
                                 folder: ROOT_ID })?;
        }
        Ok(())
    }

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
                self.startup_writable = !volume.read_only;
            }
            let mut line = Text::<256>::default();
            let _ = writeln!(&mut line, "FINDER_VOLUME id={} name={}", volume.id, volume.name());
            system.log(line.as_str())?;
        }
        if self.startup == VolumeId::NONE && count > 0
        {
            let volume = files.volume_at(0)?;
            self.startup = volume.id;
            self.startup_writable = !volume.read_only;
        }
        if let Err(error) = self.refresh_trash(system)
        {
            let mut line = Text::<160>::default();
            let _ = writeln!(&mut line,
                             "FINDER_TRASH_REFRESH_FAILED status={} error={error:?}",
                             error.status());
            system.log(line.as_str())?;
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

    pub fn open(&mut self, system: &System<'_>, location: Location) -> Result<WindowId, Error>
    {
        self.desktop_active = false;
        self.sync_desktop_activation(system)?;
        if let Some(existing) = self.windows.iter().flatten().find(|w| w.location == location)
        {
            system.select_window(existing.window)?;
            self.front = existing.window;
            return Ok(existing.window);
        }
        self.desktop_active = false;
        let Some(slot) = self.windows.iter().position(Option::is_none)
        else
        {
            return Err(Error::NoMemory);
        };
        self.failures[slot].set(None);
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
        self.front = id;
        self.windows[slot] = Some(FolderWindow { location,
                                                 window: id,
                                                 scrollbars: bars.map(|bar| bar.into_id()),
                                                 ..Default::default() });
        cache::edit(slot, |folder| {
            folder.previous_count = 0;
            folder.reset(id, location);
        });
        self.draw(system, id)?;
        self.draw_desktop(system)?;
        self.repaint_location(system, location)?;
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
        let mut closed = [None; 8];
        let mut count = 0;
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
                closed[count] = Some(w.location);
                count += 1;
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
            self.front = system.window(WindowId(0)).map_or(WindowId(0), |window| window.id);
            self.draw_desktop(system)?;
            for location in closed.into_iter().flatten()
            {
                self.repaint_location(system, location)?;
            }
            system.log("FINDER_CLOSE_COMMITTED\n")?;
        }
        Ok(())
    }

    fn repaint_location(&mut self, system: &System<'_>, location: Location) -> Result<(), Error>
    {
        for slot in 0..self.windows.len()
        {
            let Some(window) = self.windows[slot]
            else
            {
                continue;
            };
            if !cache::with(slot, |folder| folder.ready)
            {
                continue;
            }
            let item = cache::with(slot, |folder| {
                folder.items[..folder.count].iter()
                                            .find(|item| item.location() == location)
                                            .copied()
            });
            if let Some(item) = item
            {
                let result =
                    self.change_selection(system,
                                          slot,
                                          window.selected,
                                          &[item.offset(-window.scroll_h, -window.scroll).bounds]);
                if let Err(error) = result
                   && !(error == Error::NotFound && system.window(window.window).is_err())
                {
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    pub fn draw(&self, system: &System<'_>, id: WindowId) -> Result<(), Error>
    {
        let result = self.draw_inner(system, id);
        if result == Err(Error::NotFound) && system.window(id).is_err()
        {
            return Ok(());
        }
        result
    }

    fn draw_inner(&self, system: &System<'_>, id: WindowId) -> Result<(), Error>
    {
        let slot = self.windows
                       .iter()
                       .position(|w| w.is_some_and(|w| w.window == id))
                       .ok_or(Error::NotFound)?;
        if let Some(error) = self.failures[slot].get()
        {
            return self.draw_failed(system, id, error);
        }
        let Some(window) = self.windows.iter().flatten().find(|w| w.window == id)
        else
        {
            return Err(Error::NotFound);
        };
        let bounds = match system.window(id)
        {
            Ok(window) => window.bounds,
            Err(Error::NotFound) => return Ok(()),
            Err(error) => return Err(error)
        };
        let width = bounds.right - bounds.left;
        let height = bounds.bottom - bounds.top;
        cache::edit(slot, |folder| folder.frame = bounds);
        let ready = cache::with(slot, |folder| folder.ready);
        if !ready
        {
            if cache::with(slot, |folder| folder.refresh_pending || folder.refreshing)
            {
                // Otherwise the same synthetic Update remains pending forever,
                // starving the idle work that finishes this refresh. Existing
                // retained pixels stay intact; publication later damages only
                // the changed items rather than repainting this window now.
                return system.windows().validate(id);
            }
            system.begin_update(id)?;
            let port = system.quickdraw().graphics(id);
            port.reset()?;
            port.colors(0xffffff, 0xffffff)?;
            port.paint_rect(Rect::new(0, 0, width, height))?;
            return system.end_update(id);
        }
        cache::edit(slot, |folder| folder.layout(width))?;
        let volume = cache::with(slot, |folder| folder.volume.unwrap());
        system.windows().set_grow_icon_clip(id, Some(grow_icon_clip(width, height)))?;
        system.begin_update(id)?;
        let graphics = system.quickdraw().graphics(id);
        let result = (|| {
            graphics.reset()?;
            let mut draw = Port { system,
                                  graphics };
            draw.fill(Rect::new(0, 0, width, height), 0xffffff)?;
            let active = !self.desktop_active && self.front == id;
            let count = cache::with(slot, |folder| folder.count);
            self.draw_items(system, slot, active, None)?;
            graphics.clip_rect(Rect::new(0, 0, width, height))?;
            info_band(system,
                      &mut draw,
                      width,
                      count as u32,
                      if window.location == Location::default() { None } else { Some(&volume) })?;
            self.refresh_chrome(system, slot, false, true, true)?;
            Ok(())
        })();
        system.end_update(id)?;
        if let Err(error) = result
        {
            self.failures[slot].set(Some(error));
            self.draw_failed(system, id, error)?;
            return Err(error);
        }
        let mut line = Text::<256>::default();
        let _ = writeln!(&mut line, "FINDER_FOLDER_READY window={}", id);
        system.log(line.as_str())
    }

    /// Full updates and selection patches share exactly the same cached item
    /// renderer. Only the mask and candidate geometry differ.
    fn draw_items(&self, system: &System<'_>, slot: usize, active: bool, mask: Option<&[Rect]>)
                  -> Result<(), Error>
    {
        let window = self.windows[slot].unwrap();
        let frame = cache::with(slot, |folder| folder.frame);
        let viewport = Rect::new(0,
                                 icon_view::INFO_HEIGHT,
                                 frame.right - frame.left - icon_view::SCROLLBAR,
                                 frame.bottom - frame.top - icon_view::SCROLLBAR);
        let graphics = system.quickdraw().graphics(window.window);
        graphics.clip_rect(viewport)?;
        let mut draw = Port { system,
                              graphics };
        if mask.is_some()
        {
            draw.fill(viewport, 0xffffff)?;
        }
        let mut candidates = Selection::EMPTY;
        cache::with(slot, |folder| {
            if let Some(mask) = mask
            {
                for &rect in mask
                {
                    let mut local = rect;
                    local.offset(window.scroll_h, window.scroll);
                    folder.intersecting(local, |index, _| candidates.set(index, true));
                }
            }
            else
            {
                let mut local = viewport;
                local.offset(window.scroll_h, window.scroll);
                folder.intersecting(local, |index, _| candidates.set(index, true));
            }
        });
        for index in candidates.iter()
        {
            let item = cache::with(slot, |folder| folder.items[index]).offset(-window.scroll_h,
                                                                              -window.scroll);
            let open = if item.entry.is_directory()
            {
                self.is_open(item.location())
            }
            else
            {
                item.running
            };
            icon_view::draw_item_measured(&mut draw,
                                          item.icon,
                                          item.icon_bounds,
                                          item.entry.name(),
                                          item.caption,
                                          State { selected: window.selected.contains(index),
                                                  active,
                                                  open,
                                                  desktop: false })?;
        }
        Ok(())
    }

    /// All gestures publish a set through this one symmetric-difference path.
    /// Offscreen changes update state without issuing any drawing request.
    fn change_selection(&mut self, system: &System<'_>, slot: usize, next: Selection,
                        extra: &[Rect])
                        -> Result<bool, Error>
    {
        let window = self.windows[slot].unwrap();
        let changed = window.selected.difference(next);
        let next_outline = self.marquee
                               .filter(|marquee| marquee.slot == slot && !marquee.band.is_empty())
                               .map(|marquee| marquee.band);
        let outline_changed = window.outline != next_outline;
        if changed.is_empty() && extra.is_empty() && !outline_changed
        {
            return Ok(false);
        }
        let mut rectangles = [Rect::ZERO; CAPACITY * 3 + 32];
        let mut length = 0;
        let frame = cache::with(slot, |folder| folder.frame);
        let clip = Rect::new(0,
                             icon_view::INFO_HEIGHT,
                             frame.right - frame.left - icon_view::SCROLLBAR,
                             frame.bottom - frame.top - icon_view::SCROLLBAR);
        let mut append = |rect: Rect| {
            let rect = Rect::new(rect.left.max(clip.left),
                                 rect.top.max(clip.top),
                                 rect.right.min(clip.right),
                                 rect.bottom.min(clip.bottom));
            if rect.left < rect.right && rect.top < rect.bottom
            {
                rectangles[length] = rect;
                length += 1;
            }
        };
        for index in changed.iter()
        {
            append(cache::with(slot, |folder| folder.items[index]).offset(-window.scroll_h,
                                                                          -window.scroll)
                                                                  .bounds);
        }
        for &rect in extra
        {
            append(rect);
        }
        if length == 0
        {
            if outline_changed
            {
                system.quickdraw().graphics(window.window).content_outline(next_outline)?;
            }
            self.windows[slot].as_mut().unwrap().outline = next_outline;
            self.windows[slot].as_mut().unwrap().selected = next;
            return Ok(true);
        }
        let update =
            system.quickdraw().graphics(window.window).masked_rectangles(&rectangles[..length])?;
        self.windows[slot].as_mut().unwrap().selected = next;
        let result = self.draw_items(system,
                                     slot,
                                     !self.desktop_active && self.front == window.window,
                                     Some(&rectangles[..length]));
        if let Err(error) = result
        {
            self.windows[slot].as_mut().unwrap().selected = window.selected;
            return Err(error);
        }
        if outline_changed
        {
            system.quickdraw().graphics(window.window).content_outline(next_outline)?;
        }
        if let Err(error) = update.commit()
        {
            self.windows[slot].as_mut().unwrap().selected = window.selected;
            return Err(error);
        }
        self.windows[slot].as_mut().unwrap().outline = next_outline;
        Ok(true)
    }

    fn draw_failed(&self, system: &System<'_>, id: WindowId, error: Error) -> Result<(), Error>
    {
        let window = system.window(id)?;
        let body = crate::ui::error_text(system, error, 1, window.title())?;
        system.begin_update(id)?;
        let result = (|| {
            let port = system.quickdraw().graphics(id);
            port.reset()?;
            port.colors(0xffffff, 0xffffff)?;
            port.paint_rect(Rect::new(0,
                                      0,
                                      window.bounds.right - window.bounds.left,
                                      window.bounds.bottom - window.bounds.top))?;
            for (index, line) in body.as_str().split('\r').enumerate()
            {
                port.text(FontSelection { family: 3,
                                          size:   9,
                                          face:   0 },
                          Point { h: 12,
                                  v: 20 + index as i16 * 16 },
                          line,
                          0)?;
            }
            Ok(())
        })();
        system.end_update(id)?;
        result
    }

    pub fn click(&mut self, system: &System<'_>, event: &Event, position: PointerPosition)
                 -> Result<(), Error>
    {
        self.press = None;
        self.marquee = None;
        let mut target: Option<(WindowId, VolumeId, Cnid)> = None;
        if event.window().is_none()
        {
            target = self.click_desktop(system, event)?;
        }
        else if let Some(slot) =
            self.windows
                .iter()
                .position(|w| w.is_some_and(|w| w.window == event.window().unwrap_or_default()))
        {
            // A failed catalog stays inert until a real change invalidates it.
            // Clicking its error view must not repeat the failing I/O.
            if self.failures[slot].get().is_some()
            {
                return Ok(());
            }
            let window = self.windows[slot].unwrap();
            self.front = window.window;
            let bounds = cache::with(slot, |folder| folder.frame);
            let p = position.in_window(window.window)?.point()?;
            if p.h >= bounds.right - bounds.left - icon_view::SCROLLBAR
               || p.v >= bounds.bottom - bounds.top - icon_view::SCROLLBAR
            {
                let ranges = self.scroll_ranges(slot);
                for (index, id) in window.scrollbars.into_iter().enumerate()
                {
                    let tracked =
                        system.controls()
                              .control(id)
                              .track_at(position.in_window(window.window)?, |action| {
                                  let mut origin = Point { h: self.windows[slot].unwrap().scroll_h,
                                                           v: self.windows[slot].unwrap().scroll };
                                  if index == 1
                                  {
                                      origin.h = ranges[index].origin(action.scrollbar.value);
                                  }
                                  else
                                  {
                                      origin.v = ranges[index].origin(action.scrollbar.value);
                                  }
                                  self.scroll_to(system, slot, origin)
                              })?;
                    if tracked
                    {
                        // Tracking uses a stable range. Recompute only after it
                        // releases capture, so fitting content collapses the bar
                        // without cancelling an in-flight control gesture.
                        self.refresh_chrome(system, slot, false, true, false)?;
                        break;
                    }
                }
                return Ok(());
            }
            if !cache::with(slot, |folder| folder.ready)
            {
                // A watch can invalidate the view between a physical press and
                // delivery. Preserve that press until the new snapshot is
                // published; never re-enumerate from a pointer handler.
                let pending = &mut self.windows[slot].as_mut().unwrap().pending_clicks;
                let next =
                    pending.iter_mut().find(|click| click.is_none()).ok_or(Error::NoMemory)?;
                *next = Some((event.as_record(), position));
                return Ok(());
            }
            let mut hit = None;
            if p.v >= icon_view::INFO_HEIGHT
            {
                let content = Point { h: p.h.saturating_add(window.scroll_h),
                                      v: p.v.saturating_add(window.scroll) };
                cache::with(slot, |folder| {
                    folder.intersecting(Rect::new(content.h,
                                                  content.v,
                                                  content.h.saturating_add(1),
                                                  content.v.saturating_add(1)),
                                        |index, item| {
                                            if (item.icon_bounds.contains(content)
                                                || item.caption
                                                       .bounds(item.icon_bounds)
                                                       .contains(content))
                                               && hit.is_none_or(|(old, _)| index > old)
                                            {
                                                hit = Some((index, item));
                                            }
                                        })
                });
            }
            let shift = event.input().modifiers.contains(mac_toolbox::events::Modifiers::SHIFT);
            let mut next = window.selected;
            if let Some((index, item)) = hit
            {
                target = Some((window.window, item.entry.volume, item.entry.id));
                if shift
                {
                    next.toggle(index);
                }
                else if !next.contains(index)
                {
                    next = Selection::singleton(index);
                }
                self.press = next.contains(index).then_some(IconPress { entry:  item.location(),
                                            anchor: position.screen().point(),
                                            collapse:(!shift && window.selected.contains(index) && window.selected.len()>1).then_some((slot,index)) });
            }
            else
            {
                if !shift
                {
                    next = Selection::EMPTY;
                }
                self.marquee = Some(Marquee { slot,
                                              anchor: p,
                                              band: Rect::ZERO,
                                              base: next });
            }
            let was_desktop = self.desktop_active;
            self.desktop_active = false;
            self.change_selection(system, slot, next, &[])?;
            if was_desktop
            {
                let selected = self.desktop.selected.get();
                self.change_desktop_selection(system, selected, selected)?;
            }
        }

        let double = !event.input().modifiers.contains(mac_toolbox::events::Modifiers::SHIFT)
                     && self.selection_count() <= 1
                     && target.is_some_and(|(window, volume, id)| {
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
            self.press = None;
            if volume == VolumeId::NONE
            {
                self.open(system, Location::default())?;
                return Ok(());
            }
            let entry = system.files().catalog(volume, folder)?;
            self.open_entry(system, &entry)?;
        }
        system.log("FINDER_CLICK_HANDLED\n")
    }

    pub fn activate(&mut self, event: &Event)
    {
        if let Event::Activate { window,
                                 active,
                                 .. } = *event
        {
            // A desktop press deactivates the former folder asynchronously.
            // That unrelated edge must not erase the new desktop click pair.
            if active || self.last_click.is_some_and(|(target, _, _, _)| target == window)
            {
                self.last_click = None;
            }
            if !active
               && let Some(folder) =
                   self.windows.iter_mut().flatten().find(|folder| folder.window == window)
            {
                folder.pending_clicks.fill(None);
            }
            if active
            {
                self.desktop.marquee = None;
                self.desktop.pressed = None;
                self.desktop_active = false;
                self.front = window;
            }
            else if self.front == window
            {
                self.front = WindowId(0);
            }
        }
    }

    /// Movement preserves the local item layout and retained pixels. Update its
    /// screen transform on the actual geometry change, never on pointer input.
    pub fn geometry_changed(&mut self, system: &System<'_>, window: WindowId) -> Result<(), Error>
    {
        let Some(slot) =
            self.windows.iter().position(|row| row.is_some_and(|w| w.window == window))
        else
        {
            return Ok(());
        };
        let info = match system.window(window)
        {
            Ok(info) => info,
            Err(Error::NotFound) => return Ok(()), // Closed before the notification was serviced.
            Err(error) => return Err(error)
        };
        let (old_frame, extent) = cache::with(slot, |folder| (folder.frame, folder.extent));
        let window = self.windows[slot].as_mut().unwrap();
        if info.zoomed && window.zoom_restore.is_none()
        {
            window.zoom_restore = Some((old_frame,
                                        Point { h: window.scroll_h,
                                                v: window.scroll }));
            let (_, origin) = icon_view::fitted_window(extent, old_frame);
            window.scroll_h = origin.h;
            window.scroll = origin.v;
        }
        else if !info.zoomed
                  && let Some((bounds, origin)) = window.zoom_restore.take()
        {
            // A manual resize becomes a new user state. Only the actual zoom
            // restoration restores the original scroll origin as well.
            if bounds == info.bounds
            {
                window.scroll_h = origin.h;
                window.scroll = origin.v;
            }
        }
        cache::edit(slot, |folder| folder.frame = info.bounds);
        Ok(())
    }

    /// Lifecycle events supply identity and state; no catalog or process scans.
    pub fn application_file_changed(&mut self, system: &System<'_>, location: Location,
                                    running: bool)
                                    -> Result<(), Error>
    {
        let mut changed = false;
        for slot in 0..self.windows.len()
        {
            if self.windows[slot].is_none()
            {
                continue;
            }
            changed |= cache::edit(slot, |folder| {
                let mut changed = false;
                let count = folder.count;
                for item in &mut folder.items[..count]
                {
                    if item.location() == location
                       && item.running != running
                       && matches!(item.icon, Icon::Application { .. })
                    {
                        item.running = running;
                        changed = true;
                    }
                }
                changed && folder.ready
            });
        }
        if changed
        {
            self.repaint_location(system, location)?;
        }
        Ok(())
    }

    pub fn track(&mut self, system: &System<'_>, event: &Event, position: PointerPosition)
                 -> Result<(), Error>
    {
        if self.track_desktop(system, event)?
        {
            return Ok(());
        }
        if let Some(folder) =
            self.windows.iter_mut().flatten().find(|folder| folder.pending_clicks[0].is_some())
        {
            if matches!(event, Event::MouseUp { .. })
            {
                let next = folder.pending_clicks
                                 .iter_mut()
                                 .find(|event| event.is_none())
                                 .ok_or(Error::NoMemory)?;
                *next = Some((event.as_record(), position));
                return Ok(());
            }
            if matches!(event, Event::KeyDown { .. })
               && event.key().is_some_and(|key| key.code() == 0x35)
            {
                folder.pending_clicks.fill(None);
                return Ok(());
            }
        }
        if self.marquee.is_some()
        {
            return self.track_marquee(system, event, position);
        }
        if let Some(press) = self.press
           && matches!(event, Event::MouseUp { .. })
        {
            self.press = None;
            if let Some((slot, index)) = press.collapse
            {
                self.change_selection(system, slot, Selection::singleton(index), &[])?;
            }
        }
        Ok(())
    }

    fn track_marquee(&mut self, system: &System<'_>, event: &Event, position: PointerPosition)
                     -> Result<(), Error>
    {
        let marquee = self.marquee.unwrap();
        let Some(window) = self.windows[marquee.slot]
        else
        {
            self.marquee = None;
            return Ok(());
        };
        if matches!(event, Event::KeyDown { .. })
           && event.key().is_some_and(|key| key.character() == '\u{1b}')
        {
            self.marquee = None;
            return self.change_selection(system, marquee.slot, marquee.base, &[]).map(|_| ());
        }
        if !matches!(event, Event::Idle { .. } | Event::MouseUp { .. })
        {
            return Ok(());
        }
        let frame = cache::with(marquee.slot, |folder| folder.frame);
        let local = position.in_window(window.window)?;
        let point =
            Point { h: local.h()
                            .clamp(0, i32::from(frame.right - frame.left - icon_view::SCROLLBAR))
                       as i16,
                    v: local.v().clamp(i32::from(icon_view::INFO_HEIGHT),
                                       i32::from(frame.bottom - frame.top - icon_view::SCROLLBAR))
                       as i16 };
        let band = Rect::new(marquee.anchor.h.min(point.h),
                             marquee.anchor.v.min(point.v),
                             marquee.anchor.h.max(point.h),
                             marquee.anchor.v.max(point.v));
        let done = matches!(event, Event::MouseUp { .. });
        if band == marquee.band && !done
        {
            return Ok(());
        }
        let mut candidates = Selection::EMPTY;
        // Only strips entered or left by the band can change membership. The
        // row index bounds the item query; stationary interior items do no work.
        for mut rect in
            rectangle_difference(band, marquee.band).into_iter()
                                                    .chain(rectangle_difference(marquee.band, band))
        {
            if rect.is_empty()
            {
                continue;
            }
            rect.offset(window.scroll_h, window.scroll);
            cache::with(marquee.slot, |folder| {
                folder.intersecting(rect, |index, _| candidates.set(index, true))
            });
        }
        let mut next = window.selected;
        let mut content_band = band;
        content_band.offset(window.scroll_h, window.scroll);
        for index in candidates.iter()
        {
            let item = cache::with(marquee.slot, |folder| folder.items[index]);
            next.set(index,
                     marquee.base.contains(index) || cache::overlaps(item.bounds, content_band));
        }
        self.marquee = if done
        {
            None
        }
        else
        {
            Some(Marquee { band,
                           ..marquee })
        };
        self.change_selection(system, marquee.slot, next, &[]).map(|_| ())
    }

    pub fn select_all(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        if self.desktop_active
        {
            return self.change_desktop_selection(system,
                                                 Selection::all(desktop::count()),
                                                 Selection::EMPTY);
        }
        let front = system.window(WindowId(0))?.id;
        let Some(slot) = self.windows.iter().position(|w| w.is_some_and(|w| w.window == front))
        else
        {
            return Ok(());
        };
        let next = cache::with(slot, |folder| Selection::all(folder.count));
        self.change_selection(system, slot, next, &[]).map(|_| ())
    }

    pub fn open_selection(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        if self.desktop_active
        {
            let selected = self.desktop.selected.get();
            for index in selected.iter()
            {
                if let Some(item) = desktop::item(index)
                {
                    self.open(system, item.location())?;
                }
            }
            return Ok(());
        }
        let front = system.window(WindowId(0)).ok().map(|w| w.id);
        if let Some(slot) =
            self.windows.iter().position(|w| w.is_some_and(|w| Some(w.window) == front))
        {
            let selection = self.windows[slot].unwrap().selected;
            let mut entries = [None; CAPACITY];
            for index in selection.iter()
            {
                entries[index] = cache::with(slot, |folder| Some(folder.items[index].location()));
            }
            for location in entries.into_iter().flatten()
            {
                let entry = system.files().catalog(location.volume, location.folder)?;
                self.open_entry(system, &entry)?;
            }
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

    fn scroll_ranges(&self, slot: usize) -> [icon_view::ScrollRange; 2]
    {
        let window = self.windows[slot].unwrap();
        cache::with(slot, |folder| {
            let frame = folder.frame;
            let extent = folder.extent;
            if folder.count == 0
            {
                return [icon_view::ScrollRange { minimum: window.scroll,
                                                 maximum: window.scroll },
                        icon_view::ScrollRange { minimum: window.scroll_h,
                                                 maximum: window.scroll_h }];
            }
            [icon_view::ScrollRange::new(extent.top,
                                         extent.bottom,
                                         icon_view::INFO_HEIGHT,
                                         frame.bottom - frame.top - icon_view::SCROLLBAR,
                                         window.scroll),
             icon_view::ScrollRange::new(extent.left,
                                         extent.right,
                                         0,
                                         frame.right - frame.left - icon_view::SCROLLBAR,
                                         window.scroll_h)]
        })
    }

    fn configure_zoom(&self, system: &System<'_>, slot: usize) -> Result<(), Error>
    {
        let window = self.windows[slot].ok_or(Error::NotFound)?;
        let bounds =
            cache::with(slot, |folder| icon_view::fitted_window(folder.extent, folder.frame).0);
        system.windows().set_standard_bounds(window.window, Some(bounds))
    }

    fn scroll_to(&mut self, system: &System<'_>, slot: usize, origin: Point) -> Result<(), Error>
    {
        let window = self.windows[slot].ok_or(Error::NotFound)?;
        if window.scroll_h == origin.h && window.scroll == origin.v
        {
            return Ok(());
        }
        let frame = cache::with(slot, |folder| folder.frame);
        let viewport = Rect::new(0,
                                 icon_view::INFO_HEIGHT,
                                 frame.right - frame.left - icon_view::SCROLLBAR,
                                 frame.bottom - frame.top - icon_view::SCROLLBAR);
        let update = system.quickdraw().graphics(window.window).masked_rectangles(&[viewport])?;
        self.windows[slot].as_mut().unwrap().scroll_h = origin.h;
        self.windows[slot].as_mut().unwrap().scroll = origin.v;
        let result = self.draw_items(system,
                                     slot,
                                     !self.desktop_active && self.front == window.window,
                                     Some(&[viewport]))
                         .and_then(|_| update.commit());
        if result.is_err()
        {
            self.windows[slot] = Some(window);
        }
        result
    }

    pub fn scroll(&mut self, system: &System<'_>, delta: i16) -> Result<(), Error>
    {
        if let Ok(front) = system.window(WindowId(0))
           && let Some(slot) =
               self.windows.iter().position(|w| w.is_some_and(|w| w.window == front.id))
        {
            let window = self.windows[slot].unwrap();
            let range = self.scroll_ranges(slot)[0];
            let origin = Point { h: window.scroll_h,
                                 v: window.scroll
                                          .saturating_add(delta)
                                          .clamp(range.minimum, range.maximum) };
            if origin.h == window.scroll_h && origin.v == window.scroll
            {
                return Ok(());
            }
            self.scroll_to(system, slot, origin)?;
            self.refresh_chrome(system, slot, false, true, false)?;
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
        let _ = system;
        let front = self.front;
        let slot = self.windows.iter().position(|w| w.is_some_and(|w| w.window == front))?;
        let index = self.windows[slot]?.selected.iter().next()?;
        cache::with(slot, |folder| Some(folder.items[index].location()))
    }

    pub fn selection_count(&self) -> usize
    {
        if self.desktop_active
        {
            return self.desktop.selected.get().len();
        }
        self.front_slot().map_or(0, |slot| self.windows[slot].unwrap().selected.len())
    }

    pub fn selected_locations(&self) -> [Option<Location>; CAPACITY]
    {
        let mut result = [None; CAPACITY];
        if self.desktop_active
        {
            for (index, volume) in self.desktop_volumes().enumerate()
            {
                result[index] = Some(Location { volume,
                                                folder: ROOT_ID });
            }
        }
        else if let Some(slot) = self.front_slot()
        {
            for (output, index) in self.windows[slot].unwrap().selected.iter().enumerate()
            {
                result[output] = cache::with(slot, |folder| Some(folder.items[index].location()));
            }
        }
        result
    }

    pub fn info_selection(&self, system: &System<'_>) -> Option<Location>
    {
        if self.desktop_active
        {
            self.desktop_volumes().next().map(|volume| {
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
        let _ = system;
        let front = self.front;
        self.windows.iter().flatten().find(|w| w.window == front).map(|w| w.location)
    }

    fn front_slot(&self) -> Option<usize>
    {
        self.windows
            .iter()
            .position(|window| window.is_some_and(|window| window.window == self.front))
    }

    pub fn can_open(&self, _system: &System<'_>) -> bool
    {
        self.desktop_active && !self.desktop.selected.get().is_empty()
        || self.front_slot().is_some_and(|slot| !self.windows[slot].unwrap().selected.is_empty())
    }

    pub fn can_new_folder(&self, _system: &System<'_>) -> bool
    {
        if self.desktop_active
        {
            return self.startup_writable;
        }
        self.front_slot().is_some_and(|slot| {
                             cache::with(slot, |folder| {
                                 folder.location.volume != VolumeId::NONE
                                 && folder.volume.is_some_and(|volume| !volume.read_only)
                             })
                         })
    }

    pub fn can_trash(&self, _system: &System<'_>) -> bool
    {
        !self.desktop_active
        && self.front_slot().is_some_and(|slot| {
                                let selection = self.windows[slot].unwrap().selected;
                                !selection.is_empty()
                                && cache::with(slot, |folder| {
                                    selection.iter().all(|index| folder.items[index].can_trash)
                                })
                            })
    }

    pub fn can_put_away(&self, _system: &System<'_>) -> bool
    {
        !self.desktop_active
        && self.front_slot().is_some_and(|slot| {
                                let selection = self.windows[slot].unwrap().selected;
                                !selection.is_empty()
                                && cache::with(slot, |folder| {
                                    selection.iter().all(|index| folder.items[index].can_put_away)
                                })
                            })
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

    pub fn cached_trash_count(&self) -> u32
    {
        self.trash_cached.get()
    }

    fn refresh_trash(&self, system: &System<'_>) -> Result<(), Error>
    {
        self.trash_cached.set(self.trash_count(system)?);
        Ok(())
    }

    fn files_changed(&mut self, system: &System<'_>, affected: &[Location]) -> Result<(), Error>
    {
        self.refresh_trash(system)?;
        self.last_click = None;
        self.press = None;
        self.marquee = None;
        self.trash_hover = false;
        for slot in 0..self.windows.len()
        {
            let Some(window) = self.windows[slot]
            else
            {
                continue;
            };
            if window.location == Location::default() || affected.contains(&window.location)
            {
                self.failures[slot].set(None);
                self.invalidate_cache(slot);
            }
        }
        self.draw_desktop(system)
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
        self.files_changed(system, &[location])?;
        if self.desktop_active
        {
            self.open(system, location)?;
        }
        if let Some(slot) =
            self.windows
                .iter()
                .position(|entry| entry.is_some_and(|entry| entry.location == location))
        {
            cache::edit(slot, |folder| {
                folder.previous[0] = Some(Location { volume: created.volume,
                                                     folder: created.id });
                folder.previous_count = 1;
            });
        }
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
        let original = system.files().catalog(entry.volume, entry.folder)?;
        let moved = system.files().move_to_trash(entry.volume, entry.folder)?;
        self.files_changed(system,
                           &[Location { volume: entry.volume,
                                        folder: original.parent },
                             Location { volume: entry.volume,
                                        folder: moved.parent }])?;
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
        if !self.can_trash(system)
        {
            return Err(Error::BadArgument);
        }
        let selected = self.selected_locations();
        for entry in selected.into_iter().flatten()
        {
            self.move_entry_to_trash(system, entry)?;
        }
        Ok(())
    }

    pub fn put_away(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        let entry = self.selection(system).ok_or(Error::NotFound)?;
        let original = system.files().catalog(entry.volume, entry.folder)?;
        let moved = system.files().put_away(entry.volume, entry.folder)?;
        self.files_changed(system,
                           &[Location { volume: entry.volume,
                                        folder: original.parent },
                             Location { volume: entry.volume,
                                        folder: moved.parent }])?;
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
        let refresh = self.files_changed(system, &[Location::default()]);
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

fn rectangle_difference(a: Rect, b: Rect) -> [Rect; 4]
{
    if a.is_empty()
    {
        return [Rect::ZERO; 4];
    }
    if !cache::overlaps(a, b)
    {
        return [a, Rect::ZERO, Rect::ZERO, Rect::ZERO];
    }
    let hit = Rect::new(a.left.max(b.left),
                        a.top.max(b.top),
                        a.right.min(b.right),
                        a.bottom.min(b.bottom));
    [Rect::new(a.left, a.top, a.right, hit.top),
     Rect::new(a.left, hit.bottom, a.right, a.bottom),
     Rect::new(a.left, hit.top, hit.left, hit.bottom),
     Rect::new(hit.right, hit.top, a.right, hit.bottom)]
}
