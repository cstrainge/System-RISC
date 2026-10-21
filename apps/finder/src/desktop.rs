//! Finder desktop snapshots. Only explicit desktop/catalog refreshes call Files;
//! pointer paths read this bounded, synchronized application-local cache.
#![forbid(unsafe_code)]
use mac_toolbox::{Cnid, Error, InlineText, Rect, System, VolumeId, files::ROOT_ID,
                  quickdraw::fonts::FontSelection};
use spinning_top::Spinlock;

use crate::{browser::Location,
            icon_view::{Caption, Icon},
            selection::Selection};
// Resident volume namespace currently admits 128 mounts plus the virtual Trash.
pub const CAPACITY: usize = 129;
#[derive(Clone, Copy)]
pub struct Item
{
    pub volume:      VolumeId,
    pub name:        InlineText<256>,
    pub icon:        Icon,
    pub caption:     Caption,
    pub icon_bounds: Rect,
    pub bounds:      Rect
}
impl Item
{
    const EMPTY: Self = Self { volume:      VolumeId(0),
                               name:        InlineText::EMPTY,
                               icon:        Icon::Resource(0),
                               caption:     Caption::EMPTY,
                               icon_bounds: Rect::ZERO,
                               bounds:      Rect::ZERO };

    pub fn location(self) -> Location
    {
        Location { volume: self.volume,
                   folder: if self.is_trash() { Cnid::UNSAVED } else { ROOT_ID } }
    }

    pub fn is_trash(self) -> bool
    {
        self.volume == VolumeId::NONE
    }
}
struct Cache
{
    items:  [Item; CAPACITY],
    count:  usize,
    ready:  bool,
    bounds: Rect,
    index:  crate::cache::BoundsIndex<256, 512>
}
static CACHE: Spinlock<Cache> = Spinlock::new(Cache { items:  [Item::EMPTY; CAPACITY],
                                                      count:  0,
                                                      ready:  false,
                                                      bounds: Rect::ZERO,
                                                      index:  crate::cache::BoundsIndex::EMPTY });
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
    if volumes + 1 > CAPACITY
    {
        return Err(Error::NoMemory);
    }
    let previous = {
        let mut cache = CACHE.lock();
        if cache.ready && cache.count == volumes + 1
        {
            return Ok(selected);
        }
        let mut ids = [None; CAPACITY];
        for index in selected.iter()
        {
            if index < cache.count
            {
                ids[index] = Some(cache.items[index].volume);
            }
        }
        cache.ready = false;
        ids
    };
    let bounds = system.quickdraw().desktop().bounds()?;
    let mut next = Selection::EMPTY;
    let mut column = 0i16;
    let mut top = 28i16;
    let mut row = 0;
    for index in 0..=volumes
    {
        let (volume, name, icon, mut icon_bounds) = if index < volumes
        {
            let info = system.files().volume_at(index as u32)?;
            let icon = crate::browser::volume_icon(&info);
            (info.id, info.name, icon, crate::icon_view::volume_bounds(index as u32))
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
             crate::icon_view::TRASH_BOUNDS)
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
                crate::icon_view::TRASH_BOUNDS.top - 8
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
            let left = 584 - column * crate::icon_view::GRID_WIDTH;
            icon_bounds = Rect::new(left, top, left + 32, top + 32);
            top += (32 + caption.height() + 8).max(crate::icon_view::GRID_HEIGHT);
            row += 1;
        }
        let label = caption.bounds(icon_bounds);
        let item = Item { volume,
                          name,
                          icon: Icon::Resource(icon),
                          caption,
                          icon_bounds,
                          bounds: Rect::new(icon_bounds.left.min(label.left),
                                            icon_bounds.top,
                                            icon_bounds.right.max(label.right),
                                            label.bottom) };
        if previous.contains(&Some(volume))
        {
            next.set(index, true);
        }
        CACHE.lock().items[index] = item;
    }
    let mut cache = CACHE.lock();
    cache.count = volumes + 1;
    cache.bounds = bounds;
    let Cache { index,
                items,
                count,
                .. } = &mut *cache;
    index.rebuild(*count, |i| items[i].bounds);
    cache.ready = true;
    Ok(next)
}

use core::cell::Cell;

use mac_toolbox::{Event, Point, WindowId, events::Modifiers};

use super::{Browser, Port};
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
    pub selected: Cell<Selection>,
    pub marquee:  Option<Marquee>,
    outline:      Option<Rect>,
    drawn_active: Cell<bool>,
    pub pressed:  Option<(usize, Point)>
}
impl Browser
{
    pub fn draw_desktop(&self, system: &System<'_>) -> Result<(), Error>
    {
        self.desktop.selected.set(ensure(system, self.desktop.selected.get())?);
        let port = system.quickdraw().desktop();
        port.begin_update()?;
        let result = self.draw_desktop_items(system, Selection::all(count()));
        match result
        {
            Ok(()) =>
            {
                port.end_update()?;
                self.desktop.drawn_active.set(self.desktop_active);
                Ok(())
            }
            Err(error) =>
            {
                let _ = port.cancel_update();
                Err(error)
            }
        }
    }

    fn draw_desktop_items(&self, system: &System<'_>, candidates: Selection) -> Result<(), Error>
    {
        let mut draw = Port { system,
                              graphics: system.quickdraw().desktop().graphics() };
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
        let graphics = system.quickdraw().desktop().graphics();
        if length == 0
        {
            if outline_changed
            {
                graphics.content_outline(outline)?;
            }
            self.desktop.selected.set(next);
            self.desktop.outline = outline;
            self.desktop.drawn_active.set(self.desktop_active);
            return Ok(());
        }
        let update = graphics.masked_rectangles(&rectangles[..length])?;
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
                                                               graphics.content_outline(outline)?;
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

    pub(super) fn click_desktop(&mut self, system: &System<'_>, event: Event)
                                -> Result<Option<(WindowId, VolumeId, Cnid)>, Error>
    {
        let was_active = self.desktop_active;
        if !was_active
        {
            system.select_desktop()?;
        }
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
            if !shift && previous.contains(index) && previous.len() > 1
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

    pub(super) fn track_desktop(&mut self, system: &System<'_>, event: Event)
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

    pub(super) fn desktop_volumes(&self) -> impl Iterator<Item = VolumeId>
    {
        self.desktop
            .selected
            .get()
            .iter()
            .filter_map(|i| item(i).filter(|i| !i.is_trash()).map(|i| i.volume))
    }

    pub(super) fn redraw_trash_icon(&mut self, system: &System<'_>) -> Result<(), Error>
    {
        let selected = self.desktop.selected.get();
        self.change_desktop_selection(system,
                                      selected,
                                      Selection::singleton(count().saturating_sub(1)))
    }
}
