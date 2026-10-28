//! Bounded catalog projection. A scan owns only its visible rows and checked
//! cursor; large folders never become an allocation-sized UI request.
#![forbid(unsafe_code)]
use mac_abi::{files::{CatalogInfo, DirectoryScanCursor, FileScanCall, SCAN_DONE, SCAN_ENTRY,
                      SCAN_PENDING},
              *};

pub(in crate::toolbox) const ROWS: usize = 8;
pub(in crate::toolbox) const ANCESTORS: usize = 11;

/// The previous bounded viewport survives an unsuccessful type-ahead search.
/// It contains catalog values only; no provider receipt or caller buffer.
struct SavedPage
{
    rows:     [CatalogInfo; ROWS],
    length:   usize,
    first:    usize,
    selected: Option<usize>
}

pub(in crate::toolbox) struct Page
{
    pub rows:      [CatalogInfo; ROWS],
    pub length:    usize,
    pub first:     usize,
    pub total:     usize,
    pub selected:  Option<usize>,
    pub scanning:  bool,
    cursor:        DirectoryScanCursor,
    seen:          usize,
    known:         bool,
    revision:      u64,
    search:        bool,
    before_search: Option<SavedPage>
}
impl Default for Page
{
    fn default() -> Self
    {
        Self { rows:          [CatalogInfo::default(); ROWS],
               length:        0,
               first:         0,
               total:         0,
               selected:      None,
               scanning:      false,
               cursor:        DirectoryScanCursor::default(),
               seen:          0,
               known:         false,
               revision:      0,
               search:        false,
               before_search: None }
    }
}
impl Page
{
    pub fn reset(&mut self)
    {
        *self = Self { scanning: true,
                       ..Self::default() };
    }

    pub fn request(&mut self, first: usize, search: bool)
    {
        if self.first == first && !search && !self.scanning
        {
            return;
        }
        self.before_search = search.then(|| {
                                       SavedPage { rows:     self.rows,
                                                   length:   self.length,
                                                   first:    self.first,
                                                   selected: self.selected }
                                   });
        self.first = first;
        self.length = 0;
        self.seen = 0;
        self.cursor = DirectoryScanCursor::default();
        self.scanning = true;
        self.search = search;
    }

    pub fn get(&self, index: usize) -> Option<&CatalogInfo>
    {
        let local = index.checked_sub(self.first)?;
        (local < self.length).then(|| &self.rows[local])
    }

    pub fn count(&self) -> usize
    {
        // Do not rebuild the scrollbar on every offscreen scan result while
        // counting a filtered folder. Publish its exact extent at completion.
        if self.known { self.total } else { self.first + self.length }
    }

    pub fn wants_entry(&self, capacity: usize, include: bool, prefix: bool) -> bool
    {
        include
        && self.length < capacity
        && if self.search { prefix } else { self.seen >= self.first }
    }

    pub fn ready(&self, capacity: usize) -> bool
    {
        if self.known
        {
            self.length >= capacity.min(self.total.saturating_sub(self.first))
        }
        else
        {
            self.length != 0 || !self.scanning
        }
    }

    pub fn desktop_total(&mut self, total: usize)
    {
        self.known = true;
        self.total = total;
        self.scanning = false;
    }

    pub fn query(&self, volume: u32, directory: u32) -> FileScanCall
    {
        FileScanCall { operation: mac_abi::files::SCAN_DIRECTORY,
                       volume,
                       directory,
                       cursor: self.cursor,
                       ..Default::default() }
    }

    /// Returns true only when visible rows or the committed extent changed.
    pub fn accept(&mut self, reply: FileScanCall, capacity: usize, include: bool,
                  prefix_match: bool)
                  -> Result<bool, Status>
    {
        if !self.scanning || capacity == 0 || capacity > ROWS
        {
            return Err(BAD_ABI);
        }
        if self.revision != 0 && self.revision != reply.cursor.revision
        {
            self.known = false;
            self.total = 0;
            self.selected = None;
        }
        self.revision = reply.cursor.revision;
        self.cursor = reply.cursor;
        match reply.step
        {
            SCAN_PENDING => Ok(false),
            SCAN_ENTRY =>
            {
                if !include
                {
                    return Ok(false);
                }
                let index = self.seen;
                self.seen = self.seen.checked_add(1).ok_or(BAD_ABI)?;
                if self.search
                {
                    if !prefix_match
                    {
                        return Ok(false);
                    }
                    self.search = false;
                    self.before_search = None;
                    let first = if self.known
                    {
                        index.min(self.total.saturating_sub(capacity))
                    }
                    else
                    {
                        index
                    };
                    if first < index
                    {
                        self.request(first, false);
                        self.selected = Some(index);
                        return Ok(false);
                    }
                    self.first = first;
                    self.selected = Some(index);
                }
                if index < self.first || self.length >= capacity
                {
                    return Ok(false);
                }
                self.rows[self.length] = reply.entry;
                self.length += 1;
                if self.known && self.length == capacity
                {
                    self.scanning = false;
                }
                Ok(true)
            }
            SCAN_DONE =>
            {
                let changed = !self.known || self.total != self.seen;
                if self.search
                {
                    self.search = false;
                    if let Some(before) = self.before_search.take()
                    {
                        self.rows = before.rows;
                        self.length = before.length;
                        self.first = before.first;
                        self.selected = before.selected;
                    }
                }
                self.total = self.seen;
                self.known = true;
                self.scanning = false;
                if self.total != 0 && self.first > self.total.saturating_sub(capacity)
                {
                    self.request(self.total.saturating_sub(capacity), false);
                }
                if self.selected.is_some_and(|index| index >= self.total)
                {
                    self.selected = None;
                }
                Ok(changed)
            }
            _ => Err(BAD_ABI)
        }
    }
}
