//! Finder desktop snapshots. Only explicit desktop/catalog refreshes call Files;
//! pointer paths read this bounded, synchronized application-local cache.
#![forbid(unsafe_code)]
use mac_toolbox::{Cnid, Error, InlineText, Rect, System, TypeCode, VolumeId,
                  files::{DesktopCursor, ROOT_ID, ScanStep, VolumeInfo},
                  quickdraw::fonts::FontSelection};
use spinning_top::Spinlock;

use crate::{browser::Location,
            icon_view::{Caption, Icon},
            selection::Selection};
pub const CAPACITY: usize = 512;
#[derive(Clone, Copy)]
pub struct Entry
{
    pub volume:    VolumeId,
    pub id:        Cnid,
    pub parent:    Cnid,
    pub file_type: TypeCode,
    pub revision:  Option<u64>,
    directory:     bool
}
impl Entry
{
    pub fn is_directory(self) -> bool
    {
        self.directory
    }
}
#[derive(Clone, Copy)]
pub struct Item
{
    pub volume:      VolumeId,
    pub name:        InlineText<256>,
    pub icon:        Icon,
    pub caption:     Caption,
    pub icon_bounds: Rect,
    pub bounds:      Rect,
    pub entry:       Option<Entry>,
    pub read_only:   bool
}
impl Item
{
    const EMPTY: Self = Self { volume:      VolumeId(0),
                               name:        InlineText::EMPTY,
                               icon:        Icon::Resource(0),
                               caption:     Caption::EMPTY,
                               icon_bounds: Rect::ZERO,
                               bounds:      Rect::ZERO,
                               entry:       None,
                               read_only:   false };

    pub fn location(self) -> Location
    {
        Location { volume: self.volume,
                   folder: self.entry.map_or(if self.is_trash() { Cnid::UNSAVED } else { ROOT_ID },
                                             |entry| entry.id) }
    }

    pub fn is_trash(self) -> bool
    {
        self.volume == VolumeId::NONE
    }

    pub fn is_volume(self) -> bool
    {
        !self.is_trash() && self.entry.is_none()
    }
}
struct Cache
{
    items:       [Item; CAPACITY],
    count:       usize,
    ready:       bool,
    mount_count: usize,
    bounds:      Rect,
    index:       crate::cache::BoundsIndex<CAPACITY, { CAPACITY * 2 }>,
    folders:     [Option<Location>; 128]
}
static CACHE: Spinlock<Cache> = Spinlock::new(Cache { items:       [Item::EMPTY; CAPACITY],
                                                      count:       0,
                                                      ready:       false,
                                                      mount_count: 0,
                                                      bounds:      Rect::ZERO,
                                                      index:       crate::cache::BoundsIndex::EMPTY,
                                                      folders:     [None; 128] });
struct Scan
{
    pending:        bool,
    started:        bool,
    volume_index:   u32,
    volume:         Option<VolumeInfo>,
    cursor:         Option<DesktopCursor>,
    count:          usize,
    items:          [Item; CAPACITY],
    folders:        [Option<Location>; 128],
    volumes:        [Option<VolumeInfo>; 128],
    auto_cell:      usize,
    restore:        [Option<Location>; CAPACITY],
    reserved:       [Rect; CAPACITY],
    reserved_index: crate::cache::BoundsIndex<CAPACITY, { CAPACITY * 2 }>
}
static SCAN: Spinlock<Scan> = Spinlock::new(Scan { pending:        false,
                                                   started:        false,
                                                   volume_index:   0,
                                                   volume:         None,
                                                   cursor:         None,
                                                   count:          0,
                                                   items:          [Item::EMPTY; CAPACITY],
                                                   folders:        [None; 128],
                                                   volumes:        [None; 128],
                                                   auto_cell:      0,
                                                   restore:        [None; CAPACITY],
                                                   reserved:       [Rect::ZERO; CAPACITY],
                                                   reserved_index: crate::cache::BoundsIndex::EMPTY });
pub fn refresh()
{
    let mut scan = SCAN.lock();
    scan.pending = true;
    scan.started = false;
}
pub fn pending() -> bool
{
    SCAN.lock().pending
}
pub fn folders() -> [Option<Location>; 128]
{
    CACHE.lock().folders
}
pub fn folder(volume: VolumeId) -> Option<Location>
{
    CACHE.lock().folders.iter().flatten().find(|f| f.volume == volume).copied()
}
fn name(text: &str) -> Result<InlineText<256>, Error>
{
    let mut bytes = [0; 256];
    if text.len() > bytes.len()
    {
        return Err(Error::BadArgument);
    }
    bytes[..text.len()].copy_from_slice(text.as_bytes());
    InlineText::from_reply(bytes, text.len() as u32)
}
pub fn count() -> usize
{
    let cache = CACHE.lock();
    if cache.ready { cache.count } else { 0 }
}
pub fn bounds() -> Rect
{
    CACHE.lock().bounds
}
pub fn item(index: usize) -> Option<Item>
{
    let cache = CACHE.lock();
    (cache.ready && index < cache.count).then(|| cache.items[index])
}
pub fn invalidate()
{
    CACHE.lock().ready = false;
    refresh();
}
pub fn hit(point: mac_toolbox::Point) -> Option<usize>
{
    let cache = CACHE.lock();
    if !cache.ready
    {
        return None;
    }
    (0..cache.count).rev().find(|&index| {
                              let item = cache.items[index];
                              item.icon_bounds.contains(point)
                              || item.caption.bounds(item.icon_bounds).contains(point)
                          })
}
pub fn intersecting(rect: Rect, mut visit: impl FnMut(usize, Item))
{
    let cache = CACHE.lock();
    if !cache.ready
    {
        return;
    }
    cache.index.intersecting(rect, |i| cache.items[i].bounds, |i| visit(i, cache.items[i]));
}
/// Called on a desktop refresh, never during hit testing or a selection update.
/// Reconcile selection by mounted volume identity, including generation tags.
pub fn ensure(system: &System<'_>, selected: Selection) -> Result<Selection, Error>
{
    let volumes = system.files().volume_count()? as usize;
    if volumes > 128
    {
        return Err(Error::NoMemory);
    }
    let previous = {
        let mut cache = CACHE.lock();
        if cache.ready && cache.mount_count == volumes
        {
            return Ok(selected);
        }
        let mut ids = [None; CAPACITY];
        for index in selected.iter()
        {
            if index < cache.count
            {
                ids[index] = Some(cache.items[index].location());
            }
        }
        cache.ready = false;
        ids
    };
    let mut mounted = [VolumeId::NONE; CAPACITY];
    let bounds = desktop_bounds(system)?;
    let mut next = Selection::EMPTY;
    let mut column = 0i16;
    let mut top = 28i16;
    let mut row = 0;
    for index in 0..=volumes
    {
        let (volume, name, icon, mut icon_bounds, read_only) = if index < volumes
        {
            let info = system.files().volume_at(index as u32)?;
            mounted[index] = info.id;
            let icon = crate::browser::volume_icon(&info);
            (info.id, info.name, icon, Rect::ZERO, info.read_only)
        }
        else
        {
            let mut bytes = [0; 768];
            let text = crate::ui::text(system, 0, &mut bytes)?;
            if text.len() > 255
            {
                return Err(Error::BadArgument);
            }
            let mut name = [0; 256];
            name[..text.len()].copy_from_slice(text.as_bytes());
            (VolumeId::NONE,
             InlineText::from_reply(name, text.len() as u32)?,
             mac_toolbox::quickdraw::icons::standard::TRASH,
             crate::icon_view::trash_bounds(bounds),
             false)
        };
        let caption = Caption::measure_with_limit(name.as_str(), 60, |text| {
            system.quickdraw().fonts().text_width(FontSelection { family: 3,
                                                                  size:   9,
                                                                  face:   0 },
                                                  text)
        })?;
        if index < volumes
        {
            let bottom = if column == 0
            {
                crate::icon_view::trash_bounds(bounds).top - 8
            }
            else
            {
                bounds.bottom - 8
            };
            if row == 7 || top + 32 + caption.height() > bottom
            {
                column += 1;
                top = 28;
                row = 0;
            }
            icon_bounds = crate::icon_view::volume_bounds(column, top, bounds);
            top += (32 + caption.height() + 8).max(crate::icon_view::GRID_HEIGHT);
            row += 1;
        }
        let label = caption.bounds(icon_bounds);
        let item = Item { volume,
                          name: self::name(name.as_str())?,
                          icon: Icon::Resource(icon),
                          caption,
                          icon_bounds,
                          bounds: Rect::new(icon_bounds.left.min(label.left),
                                            icon_bounds.top,
                                            icon_bounds.right.max(label.right),
                                            label.bottom),
                          entry: None,
                          read_only };
        if previous.contains(&Some(item.location()))
        {
            next.set(index, true);
        }
        CACHE.lock().items[index] = item;
    }
    mounted[..volumes].sort_unstable_by_key(|id| id.0);
    super::session_positions::retain(&mounted[..volumes]);
    let mut cache = CACHE.lock();
    cache.count = volumes + 1;
    cache.mount_count = volumes;
    cache.bounds = bounds;
    cache.folders.fill(None);
    let Cache { index,
                items,
                count,
                .. } = &mut *cache;
    index.rebuild(*count, |i| items[i].bounds);
    cache.ready = true;
    drop(cache);
    refresh();
    SCAN.lock().restore = previous;
    Ok(next)
}

struct Publication
{
    selected: Selection,
    damage:   [Rect; CAPACITY * 2],
    count:    usize
}
fn advance(system: &System<'_>, selected: Selection) -> Result<Option<Publication>, Error>
{
    if !pending()
    {
        return Ok(None);
    }
    if !SCAN.lock().started
    {
        let cache = CACHE.lock();
        let mut scan = SCAN.lock();
        scan.count = 0;
        scan.volume_index = 0;
        scan.volume = None;
        scan.cursor = None;
        scan.auto_cell = 0;
        scan.folders.fill(None);
        scan.volumes.fill(None);
        for (index, item) in cache.items[..cache.count].iter().enumerate()
        {
            scan.reserved[index] = item.bounds;
        }
        let Scan { reserved_index,
                   reserved,
                   .. } = &mut *scan;
        reserved_index.rebuild(cache.count, |i| reserved[i]);
        for item in &cache.items[..cache.count]
        {
            if item.is_volume()
            {
                let at = scan.count;
                scan.items[at] = *item;
                scan.count += 1;
            }
        }
        scan.started = true;
        return Ok(None);
    }
    let (volume_index, volume, cursor) = {
        let scan = SCAN.lock();
        (scan.volume_index, scan.volume, scan.cursor)
    };
    let volume_count = system.files().volume_count()?;
    if volume_index < volume_count
    {
        if volume_index >= 128
        {
            SCAN.lock().pending = false;
            return Err(Error::NoMemory);
        }
        let Some(volume) = volume
        else
        {
            let volume = system.files().volume_at(volume_index)?;
            SCAN.lock().volume = Some(volume);
            return Ok(None);
        };
        match system.files().find_folder(volume.id, TypeCode::new(*b"desk"), !volume.read_only)
        {
            Ok(entry) =>
            {
                SCAN.lock().folders[volume_index as usize] = Some(Location { volume: entry.volume,
                                                                             folder: entry.id });
            }
            Err(Error::NotFound | Error::NotSupported) if volume.read_only => (),
            Err(error) =>
            {
                SCAN.lock().pending = false;
                return Err(error);
            }
        }
        let mut scan = SCAN.lock();
        scan.volumes[volume_index as usize] = Some(volume);
        scan.volume = None;
        scan.volume_index += 1;
        return Ok(None);
    }
    {
        let mut cursor = cursor.unwrap_or_default();
        let step = system.files().scan_desktop(&mut cursor);
        match step
        {
            Err(Error::ScanStale) =>
            {
                refresh();
                return Ok(None);
            }
            Err(error) =>
            {
                SCAN.lock().pending = false;
                return Err(error);
            }
            Ok(ScanStep::Entry { catalog: entry,
                                 .. }) =>
            {
                SCAN.lock().cursor = Some(cursor);
                if !super::visible(&entry)
                {
                    return Ok(None);
                }
                let volume = SCAN.lock()
                                 .volumes
                                 .iter()
                                 .flatten()
                                 .find(|v| v.id == entry.volume)
                                 .copied()
                                 .ok_or(Error::ScanStale)?;
                let caption = Caption::measure_with_limit(entry.name(), 60, |text| {
                    system.quickdraw().fonts().text_width(FontSelection { family: 3,
                                                                          size:   9,
                                                                          face:   0 },
                                                          text)
                })?;
                let icon = super::entry_icon(system, &volume, &entry);
                if let Icon::Application { volume,
                                           cnid,
                                           .. } = icon
                {
                    match system.quickdraw().icons().load_file(volume, cnid)
                    {
                        Ok(())
                        | Err(Error::NotFound | Error::BadArgument | Error::NotSupported) => (),
                        Err(error) => return Err(error)
                    }
                }
                let previous = {
                    let cache = CACHE.lock();
                    cache.items[..cache.count].iter()
                                              .find(|i| {
                                                  i.volume == entry.volume
                                                  && i.entry.is_some_and(|e| e.id == entry.id)
                                              })
                                              .map(|i| i.icon_bounds)
                };
                let viewport = bounds();
                let mut scan = SCAN.lock();
                if scan.count + 1 >= CAPACITY
                {
                    scan.pending = false;
                    return Err(Error::NoMemory);
                }
                let mut next_cell = scan.auto_cell;
                let mut automatic = || -> Result<Rect, Error> {
                    let columns = ((viewport.right - viewport.left - 100) / 80).max(1) as usize;
                    let rows = ((viewport.bottom - 80) / 64).max(1) as usize;
                    for cell in next_cell..columns * rows
                    {
                        let x = viewport.left
                                        .saturating_add(24)
                                        .saturating_add(((cell % columns) * 80) as i16);
                        let y = 28i16.saturating_add(((cell / columns) % rows * 64) as i16);
                        let icon = Rect::new(x, y, x + 32, y + 32);
                        let label = caption.bounds(icon);
                        let rect = Rect::new(icon.left.min(label.left),
                                             icon.top,
                                             icon.right.max(label.right),
                                             label.bottom);
                        let mut reserved = false;
                        scan.reserved_index
                            .intersecting(rect, |i| scan.reserved[i], |_| reserved = true);
                        if !reserved && !scan.items[..scan.count].iter()
                                                    .any(|i| crate::cache::overlaps(i.bounds, rect))
                        {
                            next_cell = cell + 1;
                            return Ok(icon);
                        }
                    }
                    Err(Error::NoMemory)
                };
                let icon_bounds = if let Some(point) =
                    super::session_positions::get(entry.volume, entry.id, entry.parent)
                {
                    Rect::new(point.h + 16, point.v + 28, point.h + 48, point.v + 60)
                }
                else if let Some(saved) = icon_view::saved_icon_bounds(&entry)
                {
                    saved
                }
                else if let Some(previous) = previous
                {
                    previous
                }
                else
                {
                    automatic()?
                };
                scan.auto_cell = next_cell;
                let label = caption.bounds(icon_bounds);
                let item = Item { volume: entry.volume,
                                  name: name(entry.name())?,
                                  icon,
                                  caption,
                                  icon_bounds,
                                  bounds: Rect::new(icon_bounds.left.min(label.left),
                                                    icon_bounds.top,
                                                    icon_bounds.right.max(label.right),
                                                    label.bottom),
                                  entry: Some(Entry { volume:    entry.volume,
                                                      id:        entry.id,
                                                      parent:    entry.parent,
                                                      file_type: entry.file_type,
                                                      revision:  entry.modified.mac_microseconds(),
                                                      directory: entry.is_directory() }),
                                  read_only: volume.read_only };
                let index = scan.count;
                scan.items[index] = item;
                scan.count += 1;
                return Ok(None);
            }
            Ok(ScanStep::Pending) =>
            {
                SCAN.lock().cursor = Some(cursor);
                return Ok(None);
            }
            Ok(ScanStep::Done) =>
            {}
        }
    }
    let mut publication = Publication { selected: Selection::EMPTY,
                                        damage:   [Rect::ZERO; CAPACITY * 2],
                                        count:    0 };
    let mut cache = CACHE.lock();
    let mut scan = SCAN.lock();
    let trash =
        cache.items[..cache.count].iter().find(|i| i.is_trash()).copied().ok_or(Error::BadAbi)?;
    let at = scan.count;
    scan.items[at] = trash;
    scan.count += 1;
    let mut order = [0u16; CAPACITY];
    for (i, slot) in order[..scan.count].iter_mut().enumerate()
    {
        *slot = i as u16;
    }
    order[..scan.count].sort_unstable_by_key(|i| {
                           let loc = scan.items[*i as usize].location();
                           (loc.volume.0, loc.folder.0)
                       });
    let mut matched = Selection::EMPTY;
    for (old_index, old) in cache.items[..cache.count].iter().enumerate()
    {
        let loc = old.location();
        let found = order[..scan.count].binary_search_by_key(&(loc.volume.0, loc.folder.0), |i| {
                                           let loc = scan.items[*i as usize].location();
                                           (loc.volume.0, loc.folder.0)
                                       })
                                       .ok()
                                       .map(|at| order[at] as usize);
        if let Some(index) = found
        {
            matched.set(index, true);
            publication.selected.set(index, selected.contains(old_index));
            let item = scan.items[index];
            if old.bounds == item.bounds
               && old.icon == item.icon
               && old.name == item.name
               && old.entry.map(|e| e.revision) == item.entry.map(|e| e.revision)
            {
                continue;
            }
            publication.damage[publication.count] = item.bounds;
            publication.count += 1;
        }
        publication.damage[publication.count] = old.bounds;
        publication.count += 1;
    }
    for index in 0..scan.count
    {
        if !matched.contains(index)
        {
            publication.damage[publication.count] = scan.items[index].bounds;
            publication.count += 1;
            publication.selected
                       .set(index, scan.restore.contains(&Some(scan.items[index].location())));
        }
    }
    cache.count = scan.count;
    let count = scan.count;
    cache.items[..count].copy_from_slice(&scan.items[..count]);
    cache.folders = scan.folders;
    let Cache { index,
                items,
                .. } = &mut *cache;
    index.rebuild(count, |i| items[i].bounds);
    scan.pending = false;
    scan.restore.fill(None);
    Ok(Some(publication))
}

use core::cell::Cell;

use mac_toolbox::{Event, Point, WindowId, events::Modifiers};

use super::{Browser, Port};
use mac_toolbox::{quickdraw::graphics::GraphicsPort, windows::WindowSpec};

use crate::icon_view::{self, State as IconState};
#[derive(Clone, Copy)]
pub(super) struct Marquee
{
    anchor: Point,
    band:   Rect,
    base:   Selection
}
#[derive(Default)]
pub(super) struct State
{
    pub selected:     Cell<Selection>,
    pub marquee:      Option<Marquee>,
    outline:          Option<Rect>,
    drawn_active:     Cell<bool>,
    pub pressed:      Option<(usize, Point)>,
    pub select_after: Option<Location>,
    /// The borderless stay-in-back window that hosts the desktop. Item layout,
    /// hit testing and dragging stay in global screen coordinates; only the
    /// drawing boundary translates them to this window's local coordinates.
    pub window:       WindowId,
    origin:           (i16, i16)
}

/// Draws desktop items given in global coordinates into the desktop window.
///
/// Icons are still drawn and hit-tested by Finder. The later icon-view control
/// owns keeping icons inside the visible area (no scrollbar); until then
/// Finder's own layout provides that behavior.
struct Local<'a, 's>
{
    port: Port<'a, 's>,
    dh:   i16,
    dv:   i16
}
impl Local<'_, '_>
{
    fn rect(&self, r: Rect) -> Rect
    {
        Rect::new(r.left - self.dh, r.top - self.dv, r.right - self.dh, r.bottom - self.dv)
    }
}
impl icon_view::Drawing for Local<'_, '_>
{
    fn fill(&mut self, bounds: Rect, color: u32) -> Result<(), Error>
    {
        let bounds = self.rect(bounds);
        self.port.fill(bounds, color)
    }

    fn text_width(&mut self, text: &str) -> Result<i16, Error>
    {
        self.port.text_width(text)
    }

    fn text(&mut self, baseline: Point, text: &str, color: u32) -> Result<(), Error>
    {
        let baseline = Point { h: baseline.h - self.dh,
                               v: baseline.v - self.dv };
        self.port.text(baseline, text, color)
    }

    fn icon(&mut self, id: Icon, name: &str, bounds: Rect, state: IconState)
            -> Result<(), Error>
    {
        let bounds = self.rect(bounds);
        self.port.icon(id, name, bounds, state)
    }
}
/// Classic Finder derives the desktop from the main screen below the menu bar.
pub(crate) fn desktop_bounds(system: &System<'_>) -> Result<Rect, Error>
{
    let mut bounds = system.quickdraw().screen_bounds()?;
    bounds.top = bounds.top.saturating_add(system.menus().bar_height()?);
    Ok(bounds)
}

impl Browser
{
    /// The desktop is the first window created, so it stays at the back of the
    /// creation-ordered stack even before the flag is consulted.
    pub(super) fn create_desktop_window(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        let bounds = desktop_bounds(system)?;
        self.desktop.window =
            system.windows()
                  .create(WindowSpec::new(bounds, "").procedure(15).stay_in_back())?;
        self.desktop.origin = (bounds.left, bounds.top);
        Ok(())
    }

    /// The desktop is an ordinary window: a new screen size is an ordinary
    /// resize followed by an icon re-layout and a repaint of that window.
    pub fn display_changed(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        let bounds = desktop_bounds(system)?;
        system.windows().set_bounds(self.desktop.window, bounds)?;
        self.desktop.origin = (bounds.left, bounds.top);
        invalidate();
        system.windows().invalidate(self.desktop.window, self.desktop_local(bounds))
    }

    pub fn is_desktop_window(&self, id: WindowId) -> bool
    {
        id != WindowId(0) && id == self.desktop.window
    }

    fn desktop_local(&self, r: Rect) -> Rect
    {
        let (h, v) = self.desktop.origin;
        Rect::new(r.left - h, r.top - v, r.right - h, r.bottom - v)
    }

    /// Finder paints the desktop pattern itself: the System no longer owns a
    /// desktop background. Same checker the System background used, phased by
    /// global parity (the pen pattern is aligned to the window surface).
    fn paint_desktop_pattern(&self, graphics: &GraphicsPort<'_>, local: Rect)
                             -> Result<(), Error>
    {
        let (h, v) = self.desktop.origin;
        let row = if (i32::from(h) + i32::from(v)) & 1 == 0 { 0xaa } else { 0x55 };
        graphics.colors(0x878787, 0xc0c0c0)?;
        graphics.pen_pattern([row, !row, row, !row, row, !row, row, !row])?;
        let painted = graphics.paint_rect(local);
        graphics.pen_pattern([0xff; 8])?;
        painted
    }

    fn desktop_graphics<'a>(&self, system: &System<'a>) -> GraphicsPort<'a>
    {
        system.quickdraw().graphics(self.desktop.window)
    }

    pub(super) fn advance_desktop(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        if !CACHE.lock().ready
        {
            self.draw_desktop(system)?;
        }
        let publication = match advance(system, self.desktop.selected.get())
        {
            Ok(publication) => publication,
            Err(error) =>
            {
                SCAN.lock().pending = false;
                return Err(error);
            }
        };
        let Some(publication) = publication
        else
        {
            return Ok(());
        };
        let select_after = self.desktop.select_after.and_then(|location| {
                                                        (0..count()).find(|&i| {
                                                                        item(i).is_some_and(|i| {
                                                                                   i.location()
                                                                                   == location
                                                                               })
                                                                    })
                                                    });
        self.desktop.pressed = None;
        self.desktop.marquee = None;
        self.last_click = None;
        self.desktop.selected.set(publication.selected);
        if publication.count != 0
        {
            let graphics = self.desktop_graphics(system);
            let mut local = publication.damage;
            for rect in &mut local[..publication.count]
            {
                *rect = self.desktop_local(*rect);
            }
            let update = graphics.masked_rectangles(&local[..publication.count])?;
            for &rect in &local[..publication.count]
            {
                self.paint_desktop_pattern(&graphics, rect)?;
            }
            let mut candidates = Selection::EMPTY;
            for &rect in &publication.damage[..publication.count]
            {
                intersecting(rect, |i, _| candidates.set(i, true));
            }
            self.draw_desktop_items(system, candidates)?;
            update.commit()?;
        }
        if let Some(index) = select_after
        {
            self.change_desktop_selection(system, Selection::singleton(index), Selection::EMPTY)?;
            self.desktop.select_after = None;
        }
        Ok(())
    }

    pub fn draw_desktop(&self, system: &System<'_>) -> Result<(), Error>
    {
        self.desktop.selected.set(ensure(system, self.desktop.selected.get())?);
        let id = self.desktop.window;
        system.begin_update(id)?;
        let graphics = self.desktop_graphics(system);
        let full = bounds();
        let result = (|| {
            graphics.reset()?;
            self.paint_desktop_pattern(&graphics, self.desktop_local(full))?;
            self.draw_desktop_items(system, Selection::all(count()))
        })();
        system.end_update(id)?;
        result?;
        self.desktop.drawn_active.set(self.desktop_active);
        Ok(())
    }

    fn draw_desktop_items(&self, system: &System<'_>, candidates: Selection) -> Result<(), Error>
    {
        let mut draw = Local { port: Port { system,
                                            graphics: self.desktop_graphics(system) },
                               dh:   self.desktop.origin.0,
                               dv:   self.desktop.origin.1 };
        for index in candidates.iter()
        {
            let Some(item) = item(index)
            else
            {
                continue;
            };
            let trash = item.is_trash();
            let icon = if trash && self.cached_trash_count() > 0
            {
                Icon::Resource(mac_toolbox::quickdraw::icons::standard::FULL_TRASH)
            }
            else
            {
                item.icon
            };
            icon_view::draw_item_measured(&mut draw,
                                          icon,
                                          item.icon_bounds,
                                          item.name.as_str(),
                                          item.caption,
                                          IconState { selected: self.desktop
                                                                    .selected
                                                                    .get()
                                                                    .contains(index)
                                                                || trash && self.trash_hover,
                                                      active:   self.desktop_active
                                                                || trash && self.trash_hover,
                                                      open:     self.is_open(item.location()),
                                                      desktop:  true })?;
        }
        Ok(())
    }

    pub(super) fn change_desktop_selection(&mut self, system: &System<'_>, next: Selection,
                                           forced: Selection)
                                           -> Result<(), Error>
    {
        let previous = self.desktop.selected.get();
        if previous != next
        {
            SCAN.lock().restore.fill(None);
        }
        let mut dirty = previous.difference(next);
        for index in forced.iter()
        {
            dirty.set(index, true);
        }
        let outline = self.desktop.marquee.filter(|m| !m.band.is_empty()).map(|m| m.band);
        let outline_changed = self.desktop.outline != outline;
        if dirty.is_empty() && !outline_changed
        {
            self.desktop.drawn_active.set(self.desktop_active);
            return Ok(());
        }
        let viewport = bounds();
        let mut rectangles = [Rect::ZERO; CAPACITY];
        let mut length = 0;
        for index in dirty.iter()
        {
            let Some(item) = item(index)
            else
            {
                continue;
            };
            let clip = Rect::new(item.bounds.left.max(viewport.left),
                                 item.bounds.top.max(viewport.top),
                                 item.bounds.right.min(viewport.right),
                                 item.bounds.bottom.min(viewport.bottom));
            if !clip.is_empty()
            {
                rectangles[length] = clip;
                length += 1;
            }
        }
        let graphics = self.desktop_graphics(system);
        let local_outline = outline.map(|r| self.desktop_local(r));
        if length == 0
        {
            if outline_changed
            {
                graphics.content_outline(local_outline)?;
            }
            self.desktop.selected.set(next);
            self.desktop.outline = outline;
            self.desktop.drawn_active.set(self.desktop_active);
            return Ok(());
        }
        let mut local = rectangles;
        for rect in &mut local[..length]
        {
            *rect = self.desktop_local(*rect);
        }
        let update = graphics.masked_rectangles(&local[..length])?;
        for &rect in &local[..length]
        {
            self.paint_desktop_pattern(&graphics, rect)?;
        }
        let mut candidates = Selection::EMPTY;
        for &rect in &rectangles[..length]
        {
            intersecting(rect, |index, _| candidates.set(index, true));
        }
        self.desktop.selected.set(next);
        let result =
            self.draw_desktop_items(system, candidates).and_then(|()| {
                                                           if outline_changed
                                                           {
                                                               graphics.content_outline(local_outline)?;
                                                           }
                                                           Ok(())
                                                       });
        if let Err(error) = result
        {
            self.desktop.selected.set(previous);
            return Err(error);
        }
        if let Err(error) = update.commit()
        {
            self.desktop.selected.set(previous);
            return Err(error);
        }
        self.desktop.outline = outline;
        self.desktop.drawn_active.set(self.desktop_active);
        Ok(())
    }

    pub fn sync_desktop_activation(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        if self.desktop.drawn_active.get() == self.desktop_active
        {
            return Ok(());
        }
        if !self.desktop_active
        {
            self.desktop.marquee = None;
            self.desktop.pressed = None;
        }
        let selected = self.desktop.selected.get();
        self.change_desktop_selection(system, selected, selected)
    }

    pub(super) fn click_desktop(&mut self, system: &System<'_>, event: &Event)
                                -> Result<Option<(WindowId, VolumeId, Cnid)>, Error>
    {
        let was_active = self.desktop_active;
        self.desktop_active = true;
        self.front = WindowId(0);
        self.desktop.marquee = None;
        self.desktop.pressed = None;
        let previous = self.desktop.selected.get();
        let mut next = previous;
        let shift = event.input().modifiers.contains(Modifiers::SHIFT);
        let target = if let Some(index) = hit(event.input().at)
        {
            if shift
            {
                next.toggle(index);
            }
            else if !next.contains(index)
            {
                next = Selection::singleton(index);
            }
            if !item(index).unwrap().is_trash() && next.contains(index)
            {
                self.desktop.pressed = Some((index, event.input().at));
            }
            let item = item(index).unwrap();
            Some((WindowId(0), item.location().volume, item.location().folder))
        }
        else
        {
            if !shift
            {
                next = Selection::EMPTY;
            }
            self.desktop.marquee = Some(Marquee { anchor: event.input().at,
                                                  band:   Rect::ZERO,
                                                  base:   next });
            None
        };
        self.change_desktop_selection(system,
                                      next,
                                      if was_active { Selection::EMPTY } else { previous })?;
        Ok(target)
    }

    pub(super) fn track_desktop(&mut self, system: &System<'_>, event: &Event)
                                -> Result<bool, Error>
    {
        if let Some((index, anchor)) = self.desktop.pressed
           && matches!(event, Event::MouseUp { .. })
        {
            self.desktop.pressed = None;
            if (i32::from(event.input().at.h) - i32::from(anchor.h)).abs() <= 4
               && (i32::from(event.input().at.v) - i32::from(anchor.v)).abs() <= 4
            {
                self.change_desktop_selection(system,
                                              Selection::singleton(index),
                                              Selection::EMPTY)?;
            }
        }
        let Some(marquee) = self.desktop.marquee
        else
        {
            return Ok(false);
        };
        if matches!(event, Event::KeyDown { .. })
           && event.key().is_some_and(|key| key.code() == 0x35)
        {
            self.desktop.marquee = None;
            self.change_desktop_selection(system, marquee.base, Selection::EMPTY)?;
            return Ok(true);
        }
        if !matches!(event, Event::Idle { .. } | Event::MouseUp { .. })
        {
            return Ok(true);
        }
        let viewport = bounds();
        let point = Point { h: event.input().at.h.clamp(viewport.left, viewport.right),
                            v: event.input().at.v.clamp(viewport.top, viewport.bottom) };
        let band = Rect::new(marquee.anchor.h.min(point.h),
                             marquee.anchor.v.min(point.v),
                             marquee.anchor.h.max(point.h),
                             marquee.anchor.v.max(point.v));
        let done = matches!(event, Event::MouseUp { .. });
        if band == marquee.band && !done
        {
            return Ok(true);
        }
        let mut candidates = Selection::EMPTY;
        for rect in super::rectangle_difference(band, marquee.band).into_iter()
            .chain(super::rectangle_difference(marquee.band, band)) {
            if !rect.is_empty() { intersecting(rect, |i, _| candidates.set(i, true)); }
        }
        let mut next = self.desktop.selected.get();
        for index in candidates.iter()
        {
            let item = item(index).unwrap();
            next.set(index,
                     marquee.base.contains(index) || crate::cache::overlaps(item.bounds, band));
        }
        self.desktop.marquee = if done
        {
            None
        }
        else
        {
            Some(Marquee { band,
                           ..marquee })
        };
        self.change_desktop_selection(system, next, Selection::EMPTY)?;
        Ok(true)
    }

    pub(super) fn redraw_trash_icon(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        let selected = self.desktop.selected.get();
        self.change_desktop_selection(system,
                                      selected,
                                      Selection::singleton(count().saturating_sub(1)))
    }
}
