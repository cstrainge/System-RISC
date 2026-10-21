//! App-private bounded catalog snapshots. Locks protect only local memory and
//! never cross Toolbox calls, allocation or event waits. Large storage is in BSS,
//! not the application thread's stack.
#![forbid(unsafe_code)]
use mac_toolbox::{Point, Rect, WindowId,
                  files::{CatalogInfo, DirectoryCursor, VolumeInfo}};
use spinning_top::Spinlock;

use crate::{browser::Location,
            icon_view::{Caption, Icon, Layout},
            selection::CAPACITY};

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
    pub window:         WindowId,
    pub location:       Location,
    pub count:          usize,
    pub cursor:         Option<DirectoryCursor>,
    pub ready:          bool,
    pub volume:         Option<VolumeInfo>,
    pub width:          i16,
    pub extent:         Point,
    // Sorted icon tops permit row-bounded hit tests and marquee edge queries.
    pub by_top:         [u16; CAPACITY],
    pub max_bottom:     [i16; CAPACITY * 2],
    pub previous:       [Option<Location>; CAPACITY],
    pub previous_count: usize,
    pub restored:       crate::selection::Selection,
    pub next:           u32,
    pub frame:          Rect,
    pub trash:          Option<Option<mac_toolbox::Cnid>>,
    pub notify_changed: bool
}
impl Metadata
{
    const fn new() -> Self
    {
        Self { window:         WindowId(0),
               location:       Location { volume: mac_toolbox::VolumeId::NONE,
                                          folder: mac_toolbox::Cnid::UNSAVED },
               count:          0,
               cursor:         None,
               ready:          false,
               volume:         None,
               width:          0,
               extent:         Point { h: 0,
                                       v: 0 },
               by_top:         [0; CAPACITY],
               max_bottom:     [i16::MIN; CAPACITY * 2],
               previous:       [None; CAPACITY],
               previous_count: 0,
               restored:       crate::selection::Selection::EMPTY,
               next:           0,
               frame:          Rect::ZERO,
               trash:          None,
               notify_changed: false }
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
    pub fn layout(&mut self, width: i16)
    {
        let metadata = &mut self.metadata;
        if metadata.width == width
        {
            return;
        }
        let mut layout = Layout::new(width);
        metadata.extent = Point::default();
        for index in 0..metadata.count
        {
            let item = &mut self.items[index];
            item.icon_bounds = crate::browser::entry_rect(&mut layout, &item.entry, item.caption);
            let label = item.caption.bounds(item.icon_bounds);
            item.bounds = Rect::new(item.icon_bounds.left.min(label.left),
                                    item.icon_bounds.top,
                                    item.icon_bounds.right.max(label.right),
                                    label.bottom);
            metadata.extent.h = metadata.extent.h.max(item.bounds.right);
            metadata.extent.v = metadata.extent.v.max(item.bounds.bottom);
            metadata.by_top[index] = index as u16;
        }
        metadata.by_top[..metadata.count].sort_unstable_by_key(|&index| {
                                             self.items[index as usize].bounds.top
                                         });
        // Variable-height captions need an interval tree, not a binary search
        // on bottoms (which cease to be sorted when tops are sorted).
        metadata.max_bottom.fill(i16::MIN);
        for at in 0..metadata.count
        {
            metadata.max_bottom[CAPACITY + at] =
                self.items[metadata.by_top[at] as usize].bounds.bottom;
        }
        for node in (1..CAPACITY).rev()
        {
            metadata.max_bottom[node] =
                metadata.max_bottom[node * 2].max(metadata.max_bottom[node * 2 + 1]);
        }
        metadata.width = width;
    }

    /// O(log n + intersecting rows), including unusually tall saved captions.
    pub fn intersecting(&self, rect: Rect, mut visit: impl FnMut(usize, Item))
    {
        self.visit_intervals(1, 0, CAPACITY, rect, &mut visit);
    }

    fn visit_intervals(&self, node: usize, begin: usize, end: usize, rect: Rect,
                       visit: &mut impl FnMut(usize, Item))
    {
        if begin >= self.count
           || self.max_bottom[node] <= rect.top
           || self.items[self.by_top[begin] as usize].bounds.top >= rect.bottom
        {
            return;
        }
        if end - begin == 1
        {
            let index = self.by_top[begin] as usize;
            let item = self.items[index];
            if overlaps(item.bounds, rect)
            {
                visit(index, item);
            }
            return;
        }
        let mid = begin + (end - begin) / 2;
        self.visit_intervals(node * 2, begin, mid, rect, visit);
        self.visit_intervals(node * 2 + 1, mid, end, rect, visit);
    }
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
