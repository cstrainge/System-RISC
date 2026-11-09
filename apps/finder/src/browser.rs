//! Finder catalog windows. Every volume, entry and fork comes from the File Manager.
use core::{cell::Cell, fmt::Write};
#[path = "desktop.rs"]
pub(crate) mod desktop;
#[path = "dragging.rs"]
pub(crate) mod dragging;
#[path = "session_positions.rs"]
pub(crate) mod session_positions;

use mac_toolbox::{System,
                  files::{CatalogInfo, FolderState, ROOT_ID, VolumeInfo},
                  quickdraw::{fonts::FontSelection,
                              graphics::GraphicsPort,
                              icons::{IconFlags, standard}},
                  *};

use crate::{cache,
            icon_view::{self, Caption, Drawing, Icon, Layout, State},
            selection::{CAPACITY, Selection}};

/// How many consecutive `ScanStale` restarts a folder gets before the browser
/// reports the enumeration as failed instead of restarting again. Staleness is
/// ordinary and expected under mutation; a run of them with no entry accepted
/// means the folder is changing faster than it can be read, which is a failure
/// worth showing rather than a scan to repeat silently.
const STALE_RESTART_LIMIT: u32 = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Location
{
    pub volume: VolumeId,
    pub folder: Cnid
}
/// A queued click is replayed after its catalog snapshot is ready. Only a
/// completed click may start a gesture or publish input/menu readiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickOutcome
{
    Completed,
    Deferred
}

/// Open errors retain the catalog snapshot used by the actual operation.
/// Later window activation or selection cannot change the failed subject.
pub enum ActionFailure
{
    Other(Error),
    Open
    {
        item:  CatalogInfo,
        cause: Error
    }
}
impl From<Error> for ActionFailure
{
    fn from(cause: Error) -> Self
    {
        Self::Other(cause)
    }
}

#[derive(Clone, Copy, Default)]
enum Initialization
{
    #[default]
    Volumes,
    Trash,
    Desktop,
    Root,
    Complete
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Opening
{
    #[default]
    Vertical,
    Horizontal,
    Draw,
    State,
    Desktop,
    Location,
    Complete
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
    saved_state:    Option<FolderState>,
    state_due:      Option<u32>,
    state_writable: bool,
    state_view:     u16,
    pending_clicks: [Option<(mac_toolbox::events::classic::Event, PointerPosition)>; 8],
    opening:        Opening
}
#[derive(Clone, Copy)]
struct IconPress
{
    source:   Location,
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
#[derive(Clone, Copy)]
struct FolderSave
{
    location: Location,
    state:    FolderState,
    due:      u32
}
struct StartupPatch
{
    window:   WindowId,
    location: Location,
    frame:    Rect,
    origin:   Point
}
/// Retry authority names the actual failed folder, never the front window.
#[derive(Clone, Copy)]
pub(super) struct FailedFolder
{
    window:   WindowId,
    location: Location,
    cause:    Error
}
impl FailedFolder
{
    pub(super) fn window(self) -> WindowId
    {
        self.window
    }
}
#[derive(Default)]
pub struct Browser
{
    document_open:     Option<CatalogInfo>,
    front:             WindowId,
    cache_job:         Option<usize>,
    startup_writable:  bool,
    windows:           [Option<FolderWindow>; 8],
    desktop:           desktop::State,
    desktop_active:    bool,
    last_click:        Option<(WindowId, VolumeId, Cnid, u32)>,
    press:             Option<IconPress>,
    marquee:           Option<Marquee>,
    trash_hover:       bool,
    pub startup:       VolumeId,
    trash_cached:      Cell<u32>,
    failures:          [Cell<Option<Error>>; 8],
    content_dirty:     [Cell<bool>; 8],
    open_failure:      Option<FailedFolder>,
    input_cleanup:     bool,
    deferred_updates:  u8,
    closed_states:     [Option<FolderSave>; 8],
    position_requests: Cell<u8>,
    startup_step:      Initialization,
    startup_paint:     Cell<Option<u8>>,
    startup_retained:  Cell<u8>,
    startup_desktop:   Cell<bool>,
    startup_watches:   Cell<bool>,
    startup_prune:     Cell<bool>
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
                match icons.plot_file_default(self.graphics.target(),
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
                match icons.plot_file(self.graphics.target(), volume, cnid, bounds, flags)
                {
                    Ok(()) => return Ok(()),
                    Err(Error::NotFound | Error::BadArgument | Error::NotSupported) => fallback,
                    Err(e) => return Err(e)
                }
            }
        };
        icons.plot(self.graphics.target(), id, bounds, flags)
    }
}
fn visible(entry: &CatalogInfo) -> bool
{
    !entry.finder_flags().contains(files::FinderFlags::INVISIBLE)
    && !(entry.parent == ROOT_ID
         && entry.is_directory()
         && ["Trash", "Desktop Folder"].contains(&entry.name()))
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
        // The System's own folders are recognised by name; the table lives with
        // the constants it returns so every caller shares it.
        if let Some(id) = standard::for_blessed_folder(entry.name())
        {
            return id;
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
    // Extensions arrive self-decorated: like applications, an INIT file's
    // own BNDL/FREF icon family is its icon, with the generic extension
    // puzzle as fallback. The associated-type path below never opens the
    // file's own fork, so without this every extension shows the generic.
    if entry.file_type == TypeCode::new(*b"APPL") || entry.file_type == TypeCode::new(*b"INIT")
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
    pub(super) fn queue_position_save(&self, slot: usize)
    {
        let pending = cache::with(slot, |folder| {
            folder.location.volume != VolumeId::NONE
            && folder.ready
            && folder.volume.is_some_and(|volume| !volume.read_only)
            && folder.placements != Selection::EMPTY
        });
        if pending
        {
            self.position_requests.set(self.position_requests.get() | (1 << slot));
        }
    }

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
        desktop::pending()
        || self.windows.iter().enumerate().any(|(slot, window)| {
                                              window.is_some()
                                              && self.failures[slot].get().is_none()
                                              && !cache::with(slot, |folder| folder.ready)
                                          })
    }

    pub fn begin_startup_confirmation(&self)
    {
        self.startup_paint.set(Some(0));
        self.startup_retained.set(0);
        self.startup_desktop.set(false);
        self.startup_watches.set(false);
        self.startup_prune.set(false);
    }

    pub fn finish_startup_confirmation(&self)
    {
        self.startup_paint.set(None);
        self.startup_retained.set(0);
        self.startup_desktop.set(false);
        self.startup_watches.set(false);
        self.startup_prune.set(false);
    }

    fn require_startup_prune(&self)
    {
        if self.startup_paint.get().is_some()
        {
            self.startup_prune.set(true);
        }
    }

    fn retry_pending_startup_prune(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        if self.startup_prune.get()
        {
            self.prune(system)?;
        }
        Ok(())
    }

    pub fn startup_watches_polled(&self, success: bool)
    {
        if self.startup_paint.get().is_some()
        {
            self.startup_watches.set(success);
        }
    }

    fn startup_painted(&self, slot: usize, painted: bool)
    {
        if let Some(mask) = self.startup_paint.get()
        {
            let bit = 1 << slot;
            self.startup_paint.set(Some(if painted { mask | bit } else { mask & !bit }));
            if !painted
            {
                self.startup_retained.set(self.startup_retained.get() & !bit);
            }
        }
    }

    fn begin_startup_patch(&self, slot: usize) -> Option<StartupPatch>
    {
        let receipt = self.startup_paint.get().and_then(|mask| {
            let window = self.windows[slot]?;
            if mask & (1 << slot) == 0 || self.failures[slot].get().is_some()
                || window.opening != Opening::Complete { return None; }
            cache::with(slot, |folder| {
                (folder.ready && !folder.refresh_pending && !folder.refreshing
                    && folder.window == window.window && folder.location == window.location)
                    .then_some(StartupPatch { window: window.window, location: window.location,
                        frame: folder.frame,
                        origin: Point { h: window.scroll_h, v: window.scroll } })
            })
        });
        self.startup_painted(slot, false);
        receipt
    }

    fn finish_startup_patch(&self, slot: usize, receipt: Option<StartupPatch>,
                            result: Result<(), Error>)
                            -> Result<(), Error>
    {
        if self.startup_paint.get().is_none()
        {
            return result;
        }
        if let Err(error) = result
        {
            self.startup_painted(slot, false);
            self.failures[slot].set(Some(error));
            return Err(error);
        }
        if let Some(receipt) = receipt
           && self.failures[slot].get().is_none()
           && self.windows[slot].is_some_and(|window| {
                                    window.window == receipt.window
                                    && window.location == receipt.location
                                    && window.opening == Opening::Complete
                                    && window.scroll_h == receipt.origin.h
                                    && window.scroll == receipt.origin.v
                                })
           && cache::with(slot, |folder| {
               folder.window == receipt.window
               && folder.location == receipt.location
               && folder.frame == receipt.frame
               && folder.ready
               && !folder.refresh_pending
               && !folder.refreshing
           })
        {
            // The same ready baseline survived this operation, and its actual
            // changed-region, layout and chrome commits all returned success.
            self.startup_painted(slot, true);
        }
        result
    }

    /// This is a receipt for current committed content, not the absence of work.
    /// Only the pending startup handoff pays for these bounded live-ID checks.
    pub fn startup_ready(&self, system: &System<'_>) -> Result<bool, Error>
    {
        let Some(mask) = self.startup_paint.get()
        else
        {
            return Ok(false);
        };
        if !matches!(self.startup_step, Initialization::Complete)
           || !self.startup_desktop.get()
           || !self.startup_watches.get()
           || self.startup_prune.get()
        {
            return Ok(false);
        }
        for (slot, window) in self.windows.iter().enumerate()
        {
            let Some(window) = window
            else
            {
                continue;
            };
            if mask & (1 << slot) == 0
               || self.failures[slot].get().is_some()
               || !matches!(window.opening, Opening::Complete)
            {
                return Ok(false);
            }
            let Some(frame) = cache::with(slot, |folder| {
                (folder.ready
                 && !folder.refresh_pending
                 && !folder.refreshing
                 && folder.window == window.window
                 && folder.location == window.location)
                                                       .then_some(folder.frame)
            })
            else
            {
                return Ok(false);
            };
            match system.window(window.window)
            {
                Ok(live) if live.bounds == frame => (),
                Ok(_) => return Ok(false),
                Err(Error::NotFound) =>
                {
                    self.require_startup_prune();
                    return Ok(false);
                }
                Err(error) => return Err(error)
            }
        }
        Ok(true)
    }

    /// Resume one failed real content operation after its error has been handled.
    /// A scan's unpublished cursor is retained at the failed entry; a failed
    /// ready paint retries that paint, without rescanning the folder.
    pub fn retry_startup_content(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        if self.startup_prune.get()
        {
            return self.retry_pending_startup_prune(system);
        }
        if let Some(slot) = self.windows.iter().position(|window| {
                                                   window.is_some_and(|window| {
                                                             !matches!(window.opening,
                                                                       Opening::Complete)
                                                         })
                                               })
        {
            return self.finish_open(system, slot);
        }
        if matches!(self.startup_step, Initialization::Root | Initialization::Complete)
           && !self.startup_desktop.get()
        {
            return self.draw_startup_desktop(system);
        }
        let Some(slot) = self.windows.iter().enumerate().find_map(|(slot, window)| {
            (window.is_some() && self.failures[slot].get().is_some()).then_some(slot)
        }) else { return Ok(()) };
        self.failures[slot].set(None);
        if cache::with(slot, |folder| folder.ready)
        {
            self.draw(system, self.windows[slot].unwrap().window)
        }
        else
        {
            self.cache_job = Some(slot);
            self.advance_cache(system)
        }
    }

    fn draw_startup_desktop(&self, system: &System<'_>) -> Result<(), Error>
    {
        let tracking = self.startup_paint.get().is_some();
        if tracking
        {
            self.startup_desktop.set(false);
        }
        self.draw_desktop(system)?;
        if tracking
        {
            self.startup_desktop.set(true);
        }
        Ok(())
    }

    /// Return queued pointer events to the application's normal dispatcher.
    /// Browser-only replay bypasses Drag Toolbox setup and loses the gesture.
    pub fn next_deferred_pointer(&mut self) -> Option<(Event<'static>, PointerPosition)>
    {
        let slot = self.windows.iter().enumerate().find_map(|(slot, window)| {
            (window.is_some_and(|window| window.pending_clicks[0].is_some())
             && cache::with(slot, |folder| folder.ready)).then_some(slot)
        })?;
        let pending = &mut self.windows[slot].as_mut()?.pending_clicks;
        let (event, position) = pending[0].take()?;
        pending.rotate_left(1);
        Some((Event::from(event), position))
    }

    fn invalidate_cache(&mut self, slot: usize)
    {
        let retained =
            self.startup_paint
                .get()
                .is_some_and(|mask| (mask | self.startup_retained.get()) & (1 << slot) != 0);
        self.startup_painted(slot, false);
        if retained
        {
            self.startup_retained.set(self.startup_retained.get() | (1 << slot));
        }
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
        self.advance_desktop(system)?;
        let active = self.cache_job.filter(|&slot| {
                                       self.windows[slot].is_some_and(|window| {
                                                             window.opening == Opening::Complete
                                                         })
                                       && self.failures[slot].get().is_none()
                                       && !cache::with(slot, |f| f.ready)
                                   });
        let Some(slot) = active.or_else(|| {
                                   self.windows.iter().enumerate().find_map(|(slot, window)| {
                                                                      (window.is_some_and(|window| {
                                                                           window.opening
                                                                           == Opening::Complete
                                                                       })
                                                                 && self.failures[slot].get()
                                                                                       .is_none()
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
        let retry_cursor =
            self.startup_paint
                .get()
                .map(|_| cache::with(slot, |folder| (folder.cursor, folder.next)));
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
                        // The continuation is stale, not empty: discard the
                        // partial result and restart, as the scan contract
                        // requires. Restarting is bounded because a folder that
                        // is mutated faster than it can be read would otherwise
                        // resean for ever and show nothing.
                        let retries = cache::edit(slot, |folder| {
                            folder.cursor = None;
                            folder.count = 0;
                            folder.restored = Selection::EMPTY;
                            folder.stale_retries = folder.stale_retries.saturating_add(1);
                            folder.stale_retries
                        });
                        if retries <= STALE_RESTART_LIMIT
                        {
                            return Ok(());
                        }
                        cache::edit(slot, |folder| folder.stale_retries = 0);
                        // Recorded before returning so the caller sees a folder
                        // failure with a retry, not an error dropped on the floor.
                        self.failures[slot].set(Some(Error::ScanStale));
                        return Err(Error::ScanStale);
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
                let paint_icon = if let Icon::Application { volume,
                                                            cnid,
                                                            fallback } = icon
                {
                    match system.quickdraw().icons().load_file(volume, cnid)
                    {
                        Ok(()) => icon,
                        Err(Error::NotFound | Error::BadArgument | Error::NotSupported) =>
                        {
                            Icon::Resource(fallback)
                        }
                        Err(error) => return Err(error)
                    }
                }
                else
                {
                    icon
                };
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
                let session_position =
                    volume.read_only
                          .then(|| session_positions::get(entry.volume, entry.id, entry.parent))
                          .flatten();
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
                                      paint_icon,
                                      caption,
                                      running,
                                      can_trash,
                                      can_put_away,
                                      session_position: session_position.is_some()
                                                        || previous.is_some_and(|old| {
                                                                       old.session_position
                                                                   }),
                                      icon_bounds: session_position.map(|p| {
                                                                       let x =
                                                                           p.h.saturating_add(16);
                                                                       let y =
                                                                           p.v.saturating_add(28);
                                                                       Rect::new(x,
                                                                 y,
                                                                 x.saturating_add(32),
                                                                 y.saturating_add(32))
                                                                   })
                                                                   .unwrap_or_else(|| {
                                                                       previous.filter(|old| {
                                                                           old.session_position
                                                                   || (entry.saved_icon_location()
                                                                            .is_none()
                                                                       && old.entry
                                                                             .saved_icon_location()
                                                                             .is_none())
                                                                       })
                                                                       .map_or(Rect::ZERO, |old| {
                                                                           old.icon_bounds
                                                                       })
                                                                   }),
                                      bounds: Rect::ZERO };
                    folder.count += 1;
                    // Progress: the restart counter measures consecutive
                    // restarts without an entry, not the folder's lifetime.
                    folder.stale_retries = 0;
                });
            }
            else
            {
                let bounds = system.window(window.window)?.bounds;
                let restored = cache::edit(slot, |folder| {
                    folder.layout(bounds.right - bounds.left)?;
                    folder.ready = true;
                    folder.stale_retries = 0;
                    Ok::<_, Error>(folder.restored)
                })?;
                self.windows[slot].as_mut().unwrap().selected = restored;
                self.queue_position_save(slot);
                let mut layout = Text::<200>::default();
                cache::with(slot, |folder| {
                    if folder.count != 0
                    {
                        let first = folder.items[0].icon_bounds;
                        let frame = folder.frame;
                        let _ = writeln!(&mut layout,
                                         "FINDER_FOLDER_LAYOUT window={} volume={} folder={} \
                                          first={},{},{},{} frame={},{},{},{} count={}",
                                         window.window,
                                         folder.location.volume.0,
                                         folder.location.folder.0,
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
                    let _ = system.log(layout.as_str());
                }
                self.repaint_snapshot(system, slot)?;
                if cache::edit(slot, |folder| core::mem::take(&mut folder.notify_changed))
                {
                    let mut line = Text::<100>::default();
                    let _ = writeln!(&mut line, "FINDER_FOLDER_CHANGED window={}", window.window);
                    let _ = system.log(line.as_str());
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
            if let Some(retry_cursor) = retry_cursor
            {
                cache::edit(slot, |folder| {
                    if !folder.ready
                    {
                        (folder.cursor, folder.next) = retry_cursor;
                    }
                });
            }
            self.startup_painted(slot, false);
            self.failures[slot].set(Some(error));
            self.draw_failed(system, window.window, error)?;
            return Err(error);
        }
        Ok(())
    }

    #[inline(never)]
    fn repaint_snapshot(&mut self, system: &System<'_>, slot: usize) -> Result<(), Error>
    {
        let window = self.windows[slot].unwrap();
        // A failed/obsolete prior paint cannot be the retained baseline for a
        // partial snapshot update. Retry the actual content paint in that case.
        if self.content_dirty[slot].get()
           || (self.startup_paint.get().is_some() && self.startup_retained.get() & (1 << slot) == 0)
        {
            self.draw(system, window.window)?;
            cache::edit(slot, |folder| folder.refreshing = false);
            return Ok(());
        }
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
                       || old.paint_icon != item.paint_icon
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
        self.startup_painted(slot, true);
        let _ = system.log("FINDER_FOLDER_PATCHED\n");
        Ok(())
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

    fn open_entry(&mut self, system: &System<'_>, entry: &CatalogInfo)
                  -> Result<(), ActionFailure>
    {
        let result = (|| -> Result<(), Error> {
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
            else if entry.file_type == TypeCode::new(*b"CLAP")
            {
                use mac_toolbox::processes::DocumentRef;
                let volume = system.processes().current()?.volume;
                let terminal =
                    system.files().lookup(volume, ROOT_ID, "/Applications/Utility/Terminal")?;
                system.processes().open_documents(DocumentRef { volume,
                                                                 id: terminal.id },
                                                   &[DocumentRef { volume: entry.volume,
                                                                   id:     entry.id }])?;
            }
            else if crate::named_pipe::candidate(entry)
            {
                // A named pipe is opened by connecting, never by launching an
                // application: it has no fork, so no document association can
                // claim it. The File Manager performs the connect, so the writer
                // limit and endpoint ownership stay the resident ones.
                crate::named_pipe::open(system, entry)?;
            }
            else if !crate::font_suitcase::candidate(entry)
                      && let Some(mounted) = system.files().open_item(entry.volume, entry.id)?
            {
                if self.startup_paint.get().is_some()
                {
                    self.startup_desktop.set(false);
                }
                desktop::invalidate();
                self.open(system,
                          Location { volume: mounted.id,
                                     folder: ROOT_ID })?;
            }
            else
            {
                if self.document_open.is_some()
                {
                    return Err(Error::ServiceBusy);
                }
                self.document_open = Some(*entry);
            }
            Ok(())
        })();
        result.map_err(|cause| {
                  ActionFailure::Open { item: *entry,
                                        cause }
              })
    }

    pub fn take_document_open(&mut self) -> Option<CatalogInfo>
    {
        self.document_open.take()
    }

    pub fn initialize(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        self.retry_pending_startup_prune(system)?;
        if matches!(self.startup_step, Initialization::Volumes)
        {
            let files = system.files();
            let count = files.volume_count()?;
            let mut startup = VolumeId::NONE;
            let mut writable = false;
            let mut first = None;
            for index in 0..count
            {
                let volume = files.volume_at(index)?;
                if index == 0
                {
                    first = Some((volume.id, !volume.read_only));
                }
                if volume.startup
                {
                    startup = volume.id;
                    writable = !volume.read_only;
                }
                let mut line = Text::<256>::default();
                let _ =
                    writeln!(&mut line, "FINDER_VOLUME id={} name={}", volume.id, volume.name());
                let _ = system.log(line.as_str());
            }
            if startup == VolumeId::NONE
               && let Some((id, first_writable)) = first
            {
                startup = id;
                writable = first_writable;
            }
            self.startup = startup;
            self.startup_writable = writable;
            self.startup_step = Initialization::Trash;
        }
        if matches!(self.startup_step, Initialization::Trash)
        {
            self.refresh_trash(system)?;
            self.startup_step = Initialization::Desktop;
        }
        if matches!(self.startup_step, Initialization::Desktop)
        {
            self.create_desktop_window(system)?;
            self.draw_startup_desktop(system)?;
            self.startup_step = Initialization::Root;
        }
        if matches!(self.startup_step, Initialization::Root)
        {
            if self.startup != VolumeId::NONE
            {
                self.open(system,
                          Location { volume: self.startup,
                                     folder: ROOT_ID })?;
            }
            self.startup_step = Initialization::Complete;
        }
        Ok(())
    }

    fn is_open(&self, location: Location) -> bool
    {
        self.windows.iter().flatten().any(|w| w.location == location)
    }

    pub fn contains(&self, id: WindowId) -> bool
    {
        self.is_desktop_window(id) || self.windows.iter().flatten().any(|w| w.window == id)
    }

    pub(super) fn take_modal_input(&mut self)
    {
        self.marquee = None;
        self.press = None;
        self.last_click = None;
        self.desktop.marquee = None;
        self.desktop.pressed = None;
        for window in self.windows.iter_mut().flatten()
        {
            window.pending_clicks.fill(None);
        }
        self.input_cleanup = true;
    }

    pub(super) fn finish_modal_input(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        if !self.input_cleanup
        {
            return Ok(());
        }
        for slot in 0..self.windows.len()
        {
            let Some(window) = self.windows[slot].filter(|window| window.outline.is_some())
            else
            {
                continue;
            };
            match system.window(window.window)
            {
                Ok(_) =>
                {
                    self.change_selection_inner(system, slot, window.selected, &[])?;
                }
                Err(Error::NotFound) =>
                {
                    if let Some(window) = self.windows[slot].as_mut()
                    {
                        window.outline = None;
                    }
                }
                Err(cause) => return Err(cause)
            }
        }
        self.change_desktop_selection(system, self.desktop.selected.get(), Selection::EMPTY)?;
        self.input_cleanup = false;
        Ok(())
    }

    /// Retain this actual Update while a notification owns input. Validation
    /// suppresses duplicate Update delivery; the retained bit survives refusal.
    pub(super) fn defer_update(&mut self, system: &System<'_>, id: WindowId) -> Result<(), Error>
    {
        if let Some(slot) = self.windows.iter().position(|row| row.is_some_and(|w| w.window == id))
        {
            self.deferred_updates |= 1 << slot;
            system.windows().validate(id)?;
        }
        Ok(())
    }

    pub(super) fn take_deferred_update(&mut self) -> Option<WindowId>
    {
        let slot = (0..self.windows.len()).find(|slot| self.deferred_updates & (1 << slot) != 0)?;
        self.deferred_updates &= !(1 << slot);
        self.windows[slot].map(|window| window.window)
    }

    pub(super) fn retain_draw_failure(&self, id: WindowId, cause: Error)
    {
        if let Some(slot) = self.windows.iter().position(|row| row.is_some_and(|w| w.window == id))
        {
            self.content_dirty[slot].set(true);
            if self.failures[slot].get().is_none()
            {
                self.failures[slot].set(Some(cause));
            }
        }
    }

    pub(super) fn failed_folder(&self, id: WindowId) -> Option<FailedFolder>
    {
        let slot = self.windows.iter().position(|row| row.is_some_and(|w| w.window == id))?;
        let window = self.windows[slot]?;
        let cause = self.failures[slot].get()?;
        cache::with(slot, |folder| folder.window == id && folder.location == window.location)
            .then_some(FailedFolder { window: id, location: window.location, cause })
    }

    pub(super) fn failed_cache(&self) -> Option<FailedFolder>
    {
        self.failed_folder(self.windows.get(self.cache_job?)?.as_ref()?.window)
    }

    /// One genuinely acknowledged failure permits one repair of that binding.
    /// Unpublished scans restart; a ready cache repairs its pixels without I/O.
    pub(super) fn retry_failed_folder(&mut self, system: &System<'_>, failed: FailedFolder)
                                      -> Result<(), Error>
    {
        let Some(slot) = self.windows.iter().position(|row| {
                                                row.is_some_and(|w| {
                                                       w.window == failed.window
                                                       && w.location == failed.location
                                                   })
                                            })
        else
        {
            return Ok(());
        };
        if !self.failed_folder(failed.window).is_some_and(|current| current.cause == failed.cause)
        {
            return Ok(());
        }
        let live = match system.window(failed.window)
        {
            Ok(live) => live,
            Err(Error::NotFound) =>
            {
                self.prune(system)?;
                return Ok(());
            }
            Err(error) => return Err(error)
        };
        let ready = cache::with(slot, |folder| folder.ready);
        if !ready && self.windows[slot].is_some_and(|window| window.outline.is_some())
        {
            system.quickdraw().graphics(failed.window).content_outline(None)?;
        }
        self.content_dirty[slot].set(true);
        self.failures[slot].set(None);
        cache::edit(slot, |folder| folder.frame = live.bounds);
        if self.windows[slot].is_some_and(|window| window.opening != Opening::Complete)
        {
            let result = self.finish_open(system, slot);
            if let Err(error) = result
            {
                self.failures[slot].set(Some(error));
            }
            return result;
        }
        if ready
        {
            let result = self.draw(system, failed.window);
            if let Err(error) = result
            {
                self.failures[slot].set(Some(error));
            }
            return result;
        }
        // The failed entry may have advanced cursor/next before its later
        // preparation refused. Never resume beyond that unpublished entry.
        cache::edit(slot, |folder| folder.reset(failed.window, failed.location));
        if let Some(window) = self.windows[slot].as_mut()
        {
            window.selected = Selection::EMPTY;
            window.outline = None;
            window.pending_clicks.fill(None);
        }
        if self.marquee.is_some_and(|marquee| marquee.slot == slot)
        {
            self.marquee = None;
        }
        if self.press.is_some_and(|press| press.source == failed.location)
        {
            self.press = None;
        }
        if self.last_click.is_some_and(|(window, ..)| window == failed.window)
        {
            self.last_click = None;
        }
        self.cache_job = Some(slot);
        Ok(())
    }

    pub(super) fn take_open_failure(&mut self, cause: Option<Error>) -> Option<FailedFolder>
    {
        let failed = self.open_failure.take()?;
        if cause != Some(failed.cause)
        {
            return None;
        }
        let current = self.failed_folder(failed.window)?;
        (current.location == failed.location && current.cause == failed.cause).then_some(failed)
    }

    pub fn open(&mut self, system: &System<'_>, location: Location) -> Result<WindowId, Error>
    {
        self.open_failure = None;
        let result = self.open_inner(system, location);
        if let Err(cause) = result
           && let Some((slot, window)) = self.windows.iter().enumerate().find_map(|(slot, row)| {
                                                                            row.filter(|window| {
                                                         window.location == location
                                                         && window.opening != Opening::Complete
                                                     })
                                                     .map(|window| (slot, window.window))
                                                                        })
        {
            self.failures[slot].set(Some(cause));
            self.open_failure = self.failed_folder(window);
        }
        result
    }

    fn open_inner(&mut self, system: &System<'_>, location: Location) -> Result<WindowId, Error>
    {
        self.desktop_active = false;
        self.sync_desktop_activation(system)?;
        if let Some(slot) =
            self.windows.iter().position(|w| w.is_some_and(|w| w.location == location))
        {
            let existing = self.windows[slot].unwrap().window;
            system.select_window(existing)?;
            self.front = existing;
            self.finish_open(system, slot)?;
            return Ok(existing);
        }
        self.desktop_active = false;
        let Some(slot) = self.windows.iter().position(Option::is_none)
        else
        {
            return Err(Error::NoMemory);
        };
        self.failures[slot].set(None);
        self.startup_painted(slot, false);
        self.content_dirty[slot].set(false);
        self.deferred_updates &= !(1 << slot);
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
        let persisted = (!trash).then(|| FolderState::from_catalog(&folder)).flatten();
        let queued = self.closed_states
                         .iter()
                         .position(|saved| saved.is_some_and(|s| s.location == location));
        let saved = queued.and_then(|at| self.closed_states[at].map(|s| s.state)).or(persisted);
        let desktop = desktop::desktop_bounds(system)?;
        let bounds =
            saved.map(|state| {
                     let w =
                         (i32::from(state.bounds.right) - i32::from(state.bounds.left)).clamp(180,
                                                                                              i32::from(desktop.right))
                         as i16;
                     let h =
                         (i32::from(state.bounds.bottom) - i32::from(state.bounds.top)).clamp(100,
                                                                                              i32::from(desktop.bottom) - 42)
                         as i16;
                     let x = state.bounds.left.clamp(0, desktop.right - w);
                     let y = state.bounds.top.clamp(42, desktop.bottom - h);
                     Rect::new(x, y, x + w, y + h)
                 })
                 .unwrap_or(Rect::new(8 + offset, 46 + offset, 412 + offset, 234 + offset));
        let id = system.new_window(bounds,
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
        self.front = id;
        self.windows[slot] = Some(FolderWindow { location,
                                                 window: id,
                                                 scroll: saved.map_or(0, |s| s.scroll.v),
                                                 scroll_h: saved.map_or(0, |s| s.scroll.h),
                                                 saved_state: persisted,
                                                 state_view: saved.map_or(folder.folder_view()
                                                                                .unwrap_or(0),
                                                                          |s| s.view),
                                                 state_writable: !trash && !volume.read_only,
                                                 state_due: None,
                                                 ..Default::default() });
        self.startup_watches_polled(false);
        cache::edit(slot, |folder| {
            folder.previous_count = 0;
            folder.reset(id, location);
            folder.frame = bounds;
        });
        // The reopened window now owns the latest desired state. A delayed old
        // close save must not overwrite a newer geometry change afterward.
        if let Some(at) = queued
        {
            self.closed_states[at] = None;
        }
        self.finish_open(system, slot)?;
        let mut line = Text::<256>::default();
        let _ = writeln!(&mut line,
                         "FINDER_FOLDER_OPEN volume={} cnid={} window={} name={}",
                         location.volume,
                         location.folder,
                         id,
                         folder.name());
        let _ = system.log(line.as_str());
        Ok(id)
    }

    fn finish_open(&mut self, system: &System<'_>, slot: usize) -> Result<(), Error>
    {
        let window = self.windows[slot].ok_or(Error::NotFound)?;
        for (stage, index) in [(Opening::Vertical, 0), (Opening::Horizontal, 1)]
        {
            if self.windows[slot].unwrap().opening != stage
            {
                continue;
            }
            let frame = cache::with(slot, |folder| folder.frame);
            let width = frame.right - frame.left;
            let height = frame.bottom - frame.top;
            let bounds = if index == 0
            {
                vertical_scrollbar(width, height)
            }
            else
            {
                horizontal_scrollbar(width, height)
            };
            let bar = system.controls()
                            .create_scrollbar(window.window,
                                              controls::ScrollbarState { bounds,
                                                                         ..Default::default() },
                                              1,
                                              1,
                                              controls::Reflow::No)?;
            self.windows[slot].as_mut().unwrap().scrollbars[index] = bar.into_id();
            self.windows[slot].as_mut().unwrap().opening =
                if index == 0 { Opening::Horizontal } else { Opening::Draw };
        }
        if matches!(self.windows[slot].unwrap().opening, Opening::Draw)
        {
            // A previous failed initial draw is debt, not an error-frame success.
            self.failures[slot].set(None);
            self.draw(system, window.window)?;
            self.windows[slot].as_mut().unwrap().opening = Opening::State;
        }
        if matches!(self.windows[slot].unwrap().opening, Opening::State)
        {
            self.schedule_folder_state(system, slot)?;
            self.windows[slot].as_mut().unwrap().opening = Opening::Desktop;
        }
        if matches!(self.windows[slot].unwrap().opening, Opening::Desktop)
        {
            self.draw_startup_desktop(system)?;
            self.windows[slot].as_mut().unwrap().opening = Opening::Location;
        }
        if matches!(self.windows[slot].unwrap().opening, Opening::Location)
        {
            self.repaint_location(system, window.location)?;
            self.windows[slot].as_mut().unwrap().opening = Opening::Complete;
        }
        Ok(())
    }

    fn folder_state(&self, slot: usize) -> Option<FolderState>
    {
        let window = self.windows[slot]?;
        Some(FolderState { bounds: cache::with(slot, |f| f.frame),
                           scroll: Point { h: window.scroll_h,
                                           v: window.scroll },
                           view:   window.state_view })
    }

    fn schedule_folder_state(&mut self, system: &System<'_>, slot: usize) -> Result<(), Error>
    {
        let state = self.folder_state(slot);
        let window = self.windows[slot].as_mut().ok_or(Error::NotFound)?;
        if window.state_writable
        {
            window.state_due = if state == window.saved_state
            {
                None
            }
            else
            {
                Some(system.events().ticks()?.wrapping_add(12))
            };
        }
        Ok(())
    }

    fn flush_folder_state(&mut self, system: &System<'_>, slot: usize, closing: bool)
                          -> Result<(), Error>
    {
        let Some(window) = self.windows[slot]
        else
        {
            return Ok(());
        };
        if !window.state_writable
        {
            return Ok(());
        }
        let Some(due) = window.state_due
        else
        {
            return Ok(());
        };
        if !closing && !cache::with(slot, |f| f.ready)
        {
            return Ok(());
        }
        let state = self.folder_state(slot).ok_or(Error::NotFound)?;
        if window.saved_state == Some(state)
        {
            return Ok(());
        }
        if !closing && (system.events().ticks()?.wrapping_sub(due) as i32) < 0
        {
            return Ok(());
        }
        // Clear before a failed attempt so an alert does not become an idle
        // failure storm. The next real geometry change permits another attempt.
        self.windows[slot].as_mut().unwrap().state_due = None;
        match system.files().set_folder_state(window.location.volume, window.location.folder, state)
        {
            Ok(_) => (),
            Err(Error::ServiceBusy) =>
            {
                let due = system.events().ticks()?.wrapping_add(12);
                if closing
                {
                    let at =
                        self.closed_states
                            .iter()
                            .position(|saved| saved.is_some_and(|s| s.location == window.location))
                            .or_else(|| self.closed_states.iter().position(Option::is_none))
                            .ok_or(Error::NoMemory)?;
                    self.closed_states[at] = Some(FolderSave { location: window.location,
                                                               state,
                                                               due });
                }
                else
                {
                    self.windows[slot].as_mut().unwrap().state_due = Some(due);
                }
                return Ok(());
            }
            Err(error) => return Err(error)
        }
        self.windows[slot].as_mut().unwrap().saved_state = Some(state);
        Ok(())
    }

    pub fn save_folder_states(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        for slot in 0..self.windows.len()
        {
            self.flush_folder_state(system, slot, false)?;
        }
        if self.closed_states.iter().any(Option::is_some)
        {
            let now = system.events().ticks()?;
            for slot in &mut self.closed_states
            {
                let Some(saved) = *slot
                else
                {
                    continue;
                };
                if (now.wrapping_sub(saved.due) as i32) < 0
                {
                    continue;
                }
                match system.files().set_folder_state(saved.location.volume, saved.location.folder,
                                                      saved.state)
                {
                    Ok(_) => *slot = None,
                    Err(Error::ServiceBusy) => slot.as_mut().unwrap().due = now.wrapping_add(12),
                    Err(error) =>
                    {
                        *slot = None;
                        return Err(error);
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn unmount_volume(&mut self, system: &System<'_>, volume: VolumeId)
                                 -> Result<(), Error>
    {
        if volume == self.startup
        {
            return Err(Error::AccessDenied);
        }
        // Persist pending geometry before eject. A failed/busy unmount leaves
        // every window and selection intact, ready for immediate use or retry.
        for slot in 0..self.windows.len()
        {
            if let Some(window) = self.windows[slot]
               && window.location.volume == volume
               && window.state_writable
               && let Some(state) = self.folder_state(slot)
               && window.saved_state != Some(state)
            {
                system.files().set_folder_state(volume, window.location.folder, state)?;
                self.windows[slot].as_mut().unwrap().saved_state = Some(state);
                self.windows[slot].as_mut().unwrap().state_due = None;
            }
        }
        for saved in self.closed_states.iter_mut().flatten().filter(|s| s.location.volume == volume)
        {
            system.files().set_folder_state(volume, saved.location.folder, saved.state)?;
        }
        system.files().unmount(volume)?;
        for saved in &mut self.closed_states
        {
            if saved.is_some_and(|s| s.location.volume == volume)
            {
                *saved = None;
            }
        }
        if self.startup_paint.get().is_some()
        {
            self.startup_desktop.set(false);
        }
        desktop::invalidate();
        for window in self.windows.iter_mut().flatten().filter(|w| w.location.volume == volume)
        {
            // prune must never attempt persistence through the retired ID.
            window.state_writable = false;
            window.state_due = None;
            system.close_window(window.window)?;
        }
        self.prune(system)?;
        self.draw_startup_desktop(system)?;
        let mut line = Text::<96>::default();
        writeln!(&mut line, "FINDER_VOLUME_UNMOUNTED volume={volume}").map_err(|_| {
                                                                          Error::NoMemory
                                                                      })?;
        let _ = system.log(line.as_str());
        Ok(())
    }

    pub fn prune(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        // Only a genuine absent Window may retire an owned slot. Preflight the
        // whole bounded set before flushing state or dropping any identity.
        let mut closed_mask = 0u8;
        for (slot, window) in self.windows.iter().enumerate()
        {
            let Some(window) = window
            else
            {
                continue;
            };
            match system.window(window.window)
            {
                Ok(_) => (),
                Err(Error::NotFound) => closed_mask |= 1 << slot,
                Err(error) =>
                {
                    self.require_startup_prune();
                    return Err(error);
                }
            }
        }
        let front = if closed_mask != 0
        {
            match system.window(WindowId(0))
            {
                Ok(window) => Some(window.id),
                Err(Error::NotFound) => None,
                Err(error) =>
                {
                    self.require_startup_prune();
                    return Err(error);
                }
            }
        }
        else
        {
            None
        };
        if self.startup_paint.get().is_some()
        {
            self.startup_prune.set(closed_mask != 0);
        }
        let mut state_error = None;
        let mut changed = false;
        let mut closed = [None; 8];
        let mut count = 0;
        for slot in 0..self.windows.len()
        {
            if closed_mask & (1 << slot) == 0
            {
                continue;
            }
            if let Err(error) = self.flush_folder_state(system, slot, true)
            {
                state_error.get_or_insert(error);
            }
            let window = &mut self.windows[slot];
            if let Some(w) = window
            {
                let mut line = Text::<256>::default();
                let _ = writeln!(&mut line,
                                 "FINDER_FOLDER_CLOSE volume={} cnid={} window={}",
                                 w.location.volume, w.location.folder, w.window);
                let _ = system.log(line.as_str());
                closed[count] = Some(w.location);
                count += 1;
                *window = None;
                self.content_dirty[slot].set(false);
                self.deferred_updates &= !(1 << slot);
                self.startup_painted(slot, false);
                self.startup_watches_polled(false);
                changed = true;
            }
        }
        if closed_mask != 0 && self.startup_paint.get().is_some()
        {
            self.startup_prune.set(false);
        }
        if changed
        {
            let front = front.filter(|id| !self.is_desktop_window(*id));
            if front.is_none()
            {
                self.desktop_active = true;
            }
            self.front = front.unwrap_or(WindowId(0));
            self.draw_startup_desktop(system)?;
            for location in closed.into_iter().flatten()
            {
                self.repaint_location(system, location)?;
            }
            let _ = system.log("FINDER_CLOSE_COMMITTED\n");
        }
        state_error.map_or(Ok(()), Err)
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
        if let Err(error) = result
           && self.startup_paint.get().is_some()
           && let Some(slot) = self.windows.iter().position(|w| w.is_some_and(|w| w.window == id))
        {
            self.startup_painted(slot, false);
            self.failures[slot].set(Some(error));
        }
        result
    }

    fn draw_inner(&self, system: &System<'_>, id: WindowId) -> Result<(), Error>
    {
        if self.is_desktop_window(id)
        {
            return self.draw_desktop(system);
        }
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
        // The normal event pump may see this real Window after a control/tail
        // refusal. Only the opening draw stage (with both real bars) or the
        // completed Window can paint; incomplete updates cannot claim content.
        if !matches!(window.opening, Opening::Draw | Opening::Complete)
        {
            return system.windows().validate(id);
        }
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
            let painted = (|| {
                let port = system.quickdraw().graphics(id);
                port.reset()?;
                port.colors(0xffffff, 0xffffff)?;
                port.paint_rect(Rect::new(0, 0, width, height))
            })();
            let ended = system.end_update(id);
            return painted.and(ended);
        }
        if cache::edit(slot, |folder| folder.layout(width))?
        {
            self.queue_position_save(slot);
        }
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
        let ended = system.end_update(id);
        if let Err(error) = result.and(ended)
        {
            self.failures[slot].set(Some(error));
            self.draw_failed(system, id, error)?;
            return Err(error);
        }
        if self.startup_paint.get().is_some()
        {
            cache::edit(slot, |folder| folder.refreshing = false);
        }
        self.startup_painted(slot, true);
        self.content_dirty[slot].set(false);
        let mut line = Text::<256>::default();
        let _ = writeln!(&mut line, "FINDER_FOLDER_READY window={}", id);
        let _ = system.log(line.as_str());
        Ok(())
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
                                          item.paint_icon,
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
        let result = self.change_selection_inner(system, slot, next, extra);
        if let Err(error) = result
           && self.startup_paint.get().is_some()
        {
            self.startup_painted(slot, false);
            self.failures[slot].set(Some(error));
        }
        result
    }

    fn change_selection_inner(&mut self, system: &System<'_>, slot: usize, next: Selection,
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
        if let Some(slot) = self.windows.iter().position(|row| row.is_some_and(|w| w.window == id))
        {
            self.content_dirty[slot].set(true);
        }
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
        let ended = system.end_update(id);
        result.and(ended)
    }

    pub fn click(&mut self, system: &System<'_>, event: &Event, position: PointerPosition)
                 -> Result<ClickOutcome, ActionFailure>
    {
        self.press = None;
        self.marquee = None;
        let mut target: Option<(WindowId, VolumeId, Cnid)> = None;
        if event.window().is_none_or(|w| self.is_desktop_window(w))
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
                return Ok(ClickOutcome::Completed);
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
                return Ok(ClickOutcome::Completed);
            }
            if !cache::with(slot, |folder| folder.ready)
            {
                // A watch can invalidate the view between a physical press and
                // delivery. Preserve that press until the new snapshot is
                // published; never re-enumerate from a pointer handler.
                let pending = &mut self.windows[slot].as_mut().unwrap().pending_clicks;
                let next =
                    pending.iter_mut().find(|click| click.is_none()).ok_or(Error::NoMemory)?;
                *next = Some((event.as_record()?, position));
                return Ok(ClickOutcome::Deferred);
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
                self.press = next.contains(index).then_some(IconPress {
                    source: window.location,
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
                return Ok(ClickOutcome::Completed);
            }
            let entry = system.files().catalog(volume, folder)?;
            self.open_entry(system, &entry)?;
        }
        Ok(ClickOutcome::Completed)
    }

    /// The frontmost window that is not the desktop; what Close acts on.
    pub fn closable_front(&self, system: &System<'_>) -> Option<WindowId>
    {
        system.window(WindowId(0)).ok().map(|w| w.id).filter(|&id| !self.is_desktop_window(id))
    }

    pub fn activate(&mut self, event: &Event)
    {
        if let Event::Activate { window,
                                 active,
                                 .. } = *event
        {
            if self.is_desktop_window(window)
            {
                if active
                {
                    self.desktop_active = true;
                    self.front = WindowId(0);
                }
                return;
            }
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
        let old_origin = (window.scroll_h, window.scroll);
        if info.zoomed && window.zoom_restore.is_none()
        {
            window.zoom_restore = Some((old_frame,
                                        Point { h: window.scroll_h,
                                                v: window.scroll }));
            let (_, origin) = icon_view::fitted_window(extent, old_frame, desktop::bounds());
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
        let content_changed =
            old_frame.right - old_frame.left != info.bounds.right - info.bounds.left
            || old_frame.bottom - old_frame.top != info.bounds.bottom - info.bounds.top
            || old_origin != (window.scroll_h, window.scroll);
        if content_changed
        {
            self.startup_painted(slot, false);
        }
        self.schedule_folder_state(system, slot)?;
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

    pub fn track(&mut self, system: &System<'_>, event: &Event, position: PointerPosition,
                 replayed_pointer: bool)
                 -> Result<(), Error>
    {
        if self.track_desktop(system, event)?
        {
            return Ok(());
        }
        // Only newly delivered input can extend a deferred gesture. Replaying
        // one release while another remains queued must not append it again:
        // that would keep the dispatcher busy forever and starve cache/idle work.
        if !replayed_pointer
           && let Some(folder) =
               self.windows.iter_mut().flatten().find(|folder| folder.pending_clicks[0].is_some())
        {
            if matches!(event, Event::MouseUp { .. })
            {
                let next = folder.pending_clicks
                                 .iter_mut()
                                 .find(|event| event.is_none())
                                 .ok_or(Error::NoMemory)?;
                *next = Some((event.as_record()?, position));
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

    pub fn open_selection(&mut self, system: &System<'_>) -> Result<(), ActionFailure>
    {
        if self.desktop_active
        {
            let selected = self.desktop.selected.get();
            for index in selected.iter()
            {
                if let Some(item) = desktop::item(index)
                {
                    if let Some(entry) = item.entry
                    {
                        let entry = system.files().catalog(entry.volume, entry.id)?;
                        self.open_entry(system, &entry)?;
                    }
                    else
                    {
                        self.open(system, item.location())?;
                    }
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
        let bounds = cache::with(slot, |folder| {
            icon_view::fitted_window(folder.extent, folder.frame, desktop::bounds()).0
        });
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
        if result.is_ok()
        {
            self.schedule_folder_state(system, slot)?;
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
            return self.desktop
                       .selected
                       .get()
                       .iter()
                       .filter_map(desktop::item)
                       .find(|i| !i.is_trash())
                       .map(|i| i.location());
        }
        let _ = system;
        let front = self.front;
        let slot = self.windows.iter().position(|w| w.is_some_and(|w| w.window == front))?;
        cache::with(slot, |folder| {
            if !folder.ready
            {
                return folder.previous[..folder.previous_count].iter().flatten().next().copied();
            }
            self.windows[slot]?.selected.iter().next().map(|index| folder.items[index].location())
        })
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
            for (index, item) in self.desktop
                                     .selected
                                     .get()
                                     .iter()
                                     .filter_map(desktop::item)
                                     .filter(|i| !i.is_trash())
                                     .enumerate()
            {
                result[index] = Some(item.location());
            }
        }
        else if let Some(slot) = self.front_slot()
        {
            cache::with(slot, |folder| {
                // Background refresh rebuilds index-based selection, but the
                // visibly selected file identities remain usable by Get Info.
                // Never silently discard a command in that publication gap.
                if !folder.ready
                {
                    result[..folder.previous_count].copy_from_slice(&folder.previous
                                                                        [..folder.previous_count]);
                }
                else
                {
                    for (output, index) in self.windows[slot].unwrap().selected.iter().enumerate()
                    {
                        result[output] = Some(folder.items[index].location());
                    }
                }
            });
        }
        result
    }

    pub fn info_selection(&self, system: &System<'_>) -> Option<Location>
    {
        if self.desktop_active { self.selection(system) } else { self.selection(system) }
    }

    pub fn current_folder(&self, system: &System<'_>) -> Option<Location>
    {
        if self.desktop_active
        {
            return desktop::folder(self.startup);
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
            return self.startup_writable && desktop::folder(self.startup).is_some();
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
        if self.desktop_active
        {
            let selected = self.desktop.selected.get();
            return !selected.is_empty()
                   && selected.iter().all(|i| {
                                         desktop::item(i).is_some_and(|i| {
                                                             i.entry.is_some() && !i.read_only
                                                         })
                                     });
        }
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
        if desktop::folders().iter().flatten().any(|folder| affected.contains(folder))
        {
            desktop::refresh();
        }
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
        let result = self.redraw_trash_icon(system);
        if result.is_err() && self.startup_paint.get().is_some()
        {
            self.startup_desktop.set(false);
        }
        result
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
            self.desktop.select_after = Some(Location { volume: created.volume,
                                                        folder: created.id });
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
        let _ = system.log(log.as_str());
        Ok(())
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
        let _ = system.log(log.as_str());
        Ok(())
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
        let _ = system.log(log.as_str());
        Ok(())
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
            let _ = system.log(log.as_str());
            return Err(status);
        }
        refresh?;
        let mut log = Text::<80>::default();
        let _ = writeln!(&mut log, "FINDER_TRASH_EMPTIED removed={}", removed);
        let _ = system.log(log.as_str());
        Ok(())
    }
}
impl Browser
{
    pub fn trash_usage(&self, system: &System<'_>)
                       -> Result<mac_toolbox::files::TrashUsage, Error>
    {
        let mut total = mac_toolbox::files::TrashUsage { items:           0,
                                                         allocated_bytes: Some(0) };
        for index in 0..system.files().volume_count()?
        {
            let volume = system.files().volume_at(index)?;
            let usage = system.files().trash_usage(volume.id)?;
            total.items = total.items.checked_add(usage.items).ok_or(Error::BadArgument)?;
            total.allocated_bytes = match (total.allocated_bytes, usage.allocated_bytes)
            {
                (Some(a), Some(b)) => Some(a.checked_add(b).ok_or(Error::BadArgument)?),
                _ => None
            };
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
