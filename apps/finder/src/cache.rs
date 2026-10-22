//! App-private bounded catalog snapshots. Locks protect only local memory and
//! never cross Toolbox calls, allocation or event waits. Large storage is in BSS,
//! not the application thread's stack.
#![forbid(unsafe_code)]
use mac_toolbox::{Error, Point, Rect, WindowId,
                  files::{CatalogInfo, DirectoryCursor, VolumeInfo}};
use spinning_top::Spinlock;

use crate::{browser::Location,
            icon_view::{Caption, Icon, Layout},
            selection::CAPACITY};

/// Shared cached spatial index for folder and desktop icon bounds. Geometry is
/// rebuilt only when layout changes; queries do no allocation or Toolbox work.
pub struct BoundsIndex<const N: usize, const M: usize>
{
    order:      [u16; N],
    max_bottom: [i16; M],
    count:      usize
}
impl<const N: usize, const M: usize> BoundsIndex<N, M>
{
    pub const EMPTY: Self = Self { order:      [0; N],
                                   max_bottom: [0; M],
                                   count:      0 };

    pub fn rebuild(&mut self, count: usize, bounds: impl Fn(usize) -> Rect)
    {
        assert!(N.is_power_of_two() && M == N * 2 && count <= N && N <= u16::MAX as usize);
        self.count = count;
        for (i, slot) in self.order[..count].iter_mut().enumerate()
        {
            *slot = i as u16;
        }
        self.order[..count].sort_unstable_by_key(|&i| bounds(i as usize).top);
        self.max_bottom.fill(i16::MIN);
        for at in 0..count
        {
            self.max_bottom[N + at] = bounds(self.order[at] as usize).bottom;
        }
        for node in (1..N).rev()
        {
            self.max_bottom[node] = self.max_bottom[node * 2].max(self.max_bottom[node * 2 + 1]);
        }
    }

    pub fn intersecting(&self, rect: Rect, bounds: impl Fn(usize) -> Rect,
                        mut visit: impl FnMut(usize))
    {
        if !rect.is_empty()
        {
            self.visit(1, 0, N, rect, &bounds, &mut visit);
        }
    }

    fn visit(&self, node: usize, begin: usize, end: usize, rect: Rect,
             bounds: &impl Fn(usize) -> Rect, visit: &mut impl FnMut(usize))
    {
        if begin >= self.count
           || self.max_bottom[node] <= rect.top
           || bounds(self.order[begin] as usize).top >= rect.bottom
        {
            return;
        }
        if end - begin == 1
        {
            let index = self.order[begin] as usize;
            if overlaps(bounds(index), rect)
            {
                visit(index);
            }
            return;
        }
        let mid = begin + (end - begin) / 2;
        self.visit(node * 2, begin, mid, rect, bounds, visit);
        self.visit(node * 2 + 1, mid, end, rect, bounds, visit);
    }
}

#[derive(Clone, Copy)]
pub struct Item
{
    pub entry:        CatalogInfo,
    pub icon:         Icon,
    pub caption:      Caption,
    pub icon_bounds:  Rect,
    pub bounds:       Rect,
    pub running:      bool,
    pub can_trash:    bool,
    pub can_put_away: bool
}
impl Item
{
    const EMPTY: Self = Self { entry:        CatalogInfo::EMPTY,
                               icon:         Icon::Resource(0),
                               caption:      Caption::EMPTY,
                               icon_bounds:  Rect::ZERO,
                               bounds:       Rect::ZERO,
                               running:      false,
                               can_trash:    false,
                               can_put_away: false };

    pub fn location(self) -> Location
    {
        Location { volume: self.entry.volume,
                   folder: self.entry.id }
    }

    pub fn offset(mut self, h: i16, v: i16) -> Self
    {
        self.icon_bounds.offset(h, v);
        self.bounds.offset(h, v);
        self
    }
}
pub struct Metadata
{
    pub window:          WindowId,
    pub location:        Location,
    pub count:           usize,
    pub cursor:          Option<DirectoryCursor>,
    pub ready:           bool,
    pub volume:          Option<VolumeInfo>,
    pub width:           i16,
    pub extent:          Point,
    pub index:           BoundsIndex<CAPACITY, { CAPACITY * 2 }>,
    pub previous:        [Option<Location>; CAPACITY],
    pub previous_count:  usize,
    pub restored:        crate::selection::Selection,
    pub next:            u32,
    pub frame:           Rect,
    pub trash:           Option<Option<mac_toolbox::Cnid>>,
    pub refresh_pending: bool,
    pub selected_before: crate::selection::Selection,
    pub refreshing:      bool,
    pub placements:      crate::selection::Selection,
    pub notify_changed:  bool
}
impl Metadata
{
    const fn new() -> Self
    {
        Self { window:          WindowId(0),
               location:        Location { volume: mac_toolbox::VolumeId::NONE,
                                           folder: mac_toolbox::Cnid::UNSAVED },
               count:           0,
               cursor:          None,
               ready:           false,
               volume:          None,
               width:           0,
               extent:          Point { h: 0,
                                        v: 0 },
               index:           BoundsIndex::EMPTY,
               previous:        [None; CAPACITY],
               previous_count:  0,
               restored:        crate::selection::Selection::EMPTY,
               next:            0,
               frame:           Rect::ZERO,
               trash:           None,
               refresh_pending: false,
               selected_before: crate::selection::Selection::EMPTY,
               refreshing:      false,
               placements:      crate::selection::Selection::EMPTY,
               notify_changed:  false }
    }

    pub fn reset(&mut self, window: WindowId, location: Location)
    {
        self.window = window;
        self.location = location;
        self.count = 0;
        self.cursor = None;
        self.ready = false;
        self.volume = None;
        self.width = 0;
        self.extent = Point::default();
        self.restored = crate::selection::Selection::EMPTY;
        self.next = 0;
        self.trash = None;
        self.refresh_pending = false;
        self.selected_before = crate::selection::Selection::EMPTY;
        self.refreshing = false;
        self.placements = crate::selection::Selection::EMPTY;
        self.previous_count = 0;
    }
}

pub struct Folder<'a>
{
    metadata:  &'a mut Metadata,
    pub items: &'a mut [Item; CAPACITY]
}
impl core::ops::Deref for Folder<'_>
{
    type Target = Metadata;

    fn deref(&self) -> &Metadata
    {
        self.metadata
    }
}
impl core::ops::DerefMut for Folder<'_>
{
    fn deref_mut(&mut self) -> &mut Metadata
    {
        self.metadata
    }
}
impl Folder<'_>
{
    pub fn layout(&mut self, width: i16) -> Result<(), Error>
    {
        let metadata = &mut self.metadata;
        if metadata.width == width
        {
            return Ok(());
        }
        let mut occupied = crate::selection::Selection::EMPTY;
        for index in 0..metadata.count
        {
            let item = &mut self.items[index];
            if let Some(saved) = item.entry.saved_icon_location()
            {
                let left = saved.h.saturating_add(16);
                let top = saved.v.saturating_add(28);
                item.icon_bounds =
                    Rect::new(left, top, left.saturating_add(32), top.saturating_add(32));
            }
            if !item.icon_bounds.is_empty()
            {
                occupied.set(index, true);
                item.bounds = item_bounds(item.icon_bounds, item.caption);
            }
        }
        // Only saved/session placements participate in this index. Automatic
        // cells are monotone and mutually disjoint by Layout's own contract.
        metadata.index.rebuild(metadata.count, |i| {
                          if occupied.contains(i) { self.items[i].bounds } else { Rect::ZERO }
                      });
        let mut layout = Layout::new(width);
        metadata.placements = crate::selection::Selection::EMPTY;
        for index in 0..metadata.count
        {
            if occupied.contains(index)
            {
                continue;
            }
            loop
            {
                let icon = layout.place(self.items[index].caption);
                if icon.is_empty()
                {
                    return Err(Error::BadArgument);
                }
                let bounds = item_bounds(icon, self.items[index].caption);
                let mut collision = false;
                metadata.index.intersecting(bounds,
                                            |i| {
                                                if occupied.contains(i)
                                                {
                                                    self.items[i].bounds
                                                }
                                                else
                                                {
                                                    Rect::ZERO
                                                }
                                            },
                                            |_| collision = true);
                if collision
                {
                    continue;
                }
                self.items[index].icon_bounds = icon;
                self.items[index].bounds = bounds;
                metadata.placements.set(index, true);
                break;
            }
        }
        metadata.extent = Point::default();
        for item in &self.items[..metadata.count]
        {
            metadata.extent.h = metadata.extent.h.max(item.bounds.right);
            metadata.extent.v = metadata.extent.v.max(item.bounds.bottom);
        }
        metadata.index.rebuild(metadata.count, |index| self.items[index].bounds);
        metadata.width = width;
        Ok(())
    }

    /// O(log n + intersecting rows), including unusually tall saved captions.
    pub fn intersecting(&self, rect: Rect, mut visit: impl FnMut(usize, Item))
    {
        self.index.intersecting(rect,
                                |index| self.items[index].bounds,
                                |index| visit(index, self.items[index]));
    }
}
fn item_bounds(icon: Rect, caption: Caption) -> Rect
{
    let label = caption.bounds(icon);
    Rect::new(icon.left.min(label.left), icon.top, icon.right.max(label.right), label.bottom)
}

/// One shared baseline for the one active incremental scan, rather than a
/// second complete catalog per window. Pending windows retain their own cache.
pub struct Snapshot
{
    window:       WindowId,
    pub count:    usize,
    pub selected: crate::selection::Selection,
    pub capacity: Option<mac_toolbox::files::VolumeCapacity>,
    pub extent:   Point,
    pub items:    [Item; CAPACITY],
    order:        [u16; CAPACITY]
}
impl Snapshot
{
    pub fn find(&self, location: Location) -> Option<usize>
    {
        self.order[..self.count].binary_search_by_key(&(location.volume.0, location.folder.0),
                                                      |&i| {
                                                          (self.items[i as usize].entry.volume.0,
                                                           self.items[i as usize].entry.id.0)
                                                      })
                                .ok()
                                .map(|at| self.order[at] as usize)
    }
}
static SNAPSHOT: Spinlock<Snapshot> =
    Spinlock::new(Snapshot { window:   WindowId(0),
                             count:    0,
                             selected: crate::selection::Selection::EMPTY,
                             capacity: None,
                             extent:   Point { h: 0,
                                               v: 0 },
                             items:    [Item::EMPTY; CAPACITY],
                             order:    [0; CAPACITY] });
pub fn start_refresh(slot: usize)
{
    edit(slot, |folder| {
        if !folder.refresh_pending
        {
            return;
        }
        let mut previous = SNAPSHOT.lock();
        previous.window = folder.window;
        previous.count = folder.count;
        previous.selected = folder.selected_before;
        previous.capacity = folder.volume.and_then(|volume| volume.capacity);
        previous.extent = folder.extent;
        let count = folder.count;
        previous.items[..count].copy_from_slice(&folder.items[..count]);
        let Snapshot { items,
                       order,
                       .. } = &mut *previous;
        for (index, entry) in order[..count].iter_mut().enumerate()
        {
            *entry = index as u16;
        }
        order[..count].sort_unstable_by_key(|&i| {
                          (items[i as usize].entry.volume.0, items[i as usize].entry.id.0)
                      });
        let (window, location, selected_count) =
            (folder.window, folder.location, folder.previous_count);
        folder.reset(window, location);
        folder.previous_count = selected_count;
        folder.refreshing = true;
    });
}
pub fn prior(slot: usize, location: Location) -> Option<Item>
{
    compare(slot, |_, previous| previous.and_then(|p| p.find(location).map(|i| p.items[i])))
}
pub fn compare<T>(slot: usize, run: impl FnOnce(&Folder<'_>, Option<&Snapshot>) -> T) -> T
{
    with(slot, |folder| {
        let previous = SNAPSHOT.lock();
        let old = (folder.refreshing && previous.window == folder.window).then_some(&*previous);
        run(folder, old)
    })
}

pub fn overlaps(a: Rect, b: Rect) -> bool
{
    a.left < b.right && a.right > b.left && a.top < b.bottom && a.bottom > b.top
}
// Keep nonzero Option discriminants in small metadata, away from the large
// all-zero item payload. Verify .bss in native acceptance builds.
static FOLDERS: Spinlock<[Metadata; 8]> = Spinlock::new([const { Metadata::new() }; 8]);
static ITEMS: Spinlock<[[Item; CAPACITY]; 8]> = Spinlock::new([[Item::EMPTY; CAPACITY]; 8]);
pub fn with<T>(slot: usize, inspect: impl FnOnce(&Folder<'_>) -> T) -> T
{
    edit(slot, |folder| inspect(folder))
}
pub fn edit<T>(slot: usize, change: impl FnOnce(&mut Folder<'_>) -> T) -> T
{
    let mut metadata = FOLDERS.lock();
    let mut items = ITEMS.lock();
    change(&mut Folder { metadata: &mut metadata[slot],
                         items:    &mut items[slot] })
}
