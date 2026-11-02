//! Watched startup-volume font discovery; storage work advances outside drawing.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{files::*, *};
use mac_object_table::shared::InvocationGate;

use super::{Caller, Manager};
use crate::toolbox::quickdraw::fonts::{FontManager, FontSourceKey, InstalledFontData,
                                       InstalledFontSnapshot, InstalledFontSource};

const MAX_FILES: usize = 128;
const MAX_FORK_BYTES: usize = 8 * 1024 * 1024;
const MAX_SCAN_BYTES: usize = 32 * 1024 * 1024;
const READ_BYTES: usize = 32 * 1024;

struct Source
{
    entry:    CatalogInfo,
    data:     Vec<u8>,
    resource: Vec<u8>
}

struct Reading
{
    source: Source,
    handle: u32,
    fork:   u32,
    offset: usize
}

struct Refresh
{
    caller:          Caller,
    volume:          u32,
    source_volume:   u64,
    parent_watch:    u32,
    directory:       u32,
    directory_watch: u32,
    cursor:          DirectoryScanCursor,
    sources:         Vec<Source>,
    reading:         Option<Reading>,
    active:          bool,
    bytes:           usize,
    retry_at:        u64,
    error:           Option<Status>,
    baseline:        Option<alloc::sync::Arc<FontManager>>,
    snapshot:        Option<InstalledFontSnapshot>,
    parsing:         bool
}

struct State
{
    refresh:  Option<Refresh>,
    retry_at: u64,
    error:    Option<Status>
}
enum Slot
{
    Ready(State),
    Busy
}
static STATE: spinning_top::Spinlock<Slot> =
    spinning_top::Spinlock::new(Slot::Ready(State { refresh:  None,
                                                    retry_at: 0,
                                                    error:    None }));

fn files<R>(operation: impl FnOnce(&mut Manager) -> Result<R, Status>) -> Result<R, Status>
{
    crate::kernel::with_files(operation).map_err(crate::state::AccessError::status)?
}

fn call(mut request: FileCall, output: &mut [u8]) -> Result<FileCall, Status>
{
    files(|manager| {
        manager.request(&mut request, "", output)?;
        Ok(request)
    })
}

impl Refresh
{
    fn advance(&mut self)
    {
        let now = crate::platform::ticks();
        let result = (|| {
            let parent_changed = self.watch(self.parent_watch, POLL_WATCH)?;
            let changed = self.watch(self.directory_watch, POLL_WATCH)?;
            let rebound = parent_changed && self.resolve_directory()? != self.directory;
            if rebound || changed
            {
                if self.active
                {
                    return Err(SCAN_STALE);
                }
                self.retry_at = 0;
            }
            if !self.active
            {
                if let Some(reading) = self.reading.as_ref()
                {
                    if reading.handle != 0
                    {
                        call(FileCall { operation: CLOSE,
                                        handle: reading.handle,
                                        ..Default::default() },
                             &mut [])?;
                    }
                    self.reading = None;
                    self.sources.clear();
                }
                if now < self.retry_at
                {
                    return Ok(());
                }
                return self.begin();
            }
            if self.reading.is_some()
            {
                return self.read();
            }
            if self.parsing
            {
                return self.finish();
            }
            if self.directory == 0
            {
                return self.finish();
            }
            let mut request = FileScanCall { operation: SCAN_DIRECTORY,
                                             volume: self.volume,
                                             directory: self.directory,
                                             cursor: self.cursor,
                                             ..Default::default() };
            files(|manager| manager.scan_request(&mut request))?;
            self.cursor = request.cursor;
            match request.step
            {
                SCAN_ENTRY if request.entry.kind != DIRECTORY => self.open(request.entry),
                SCAN_ENTRY | SCAN_PENDING => Ok(()),
                SCAN_DONE => self.finish(),
                _ => Err(BAD_ABI)
            }
        })();
        if let Err(status) = result
        {
            if self.error != Some(status)
            {
                serial!("FONT_REFRESH_FAILED volume={} directory={} status={} retained=previous",
                        self.volume, self.directory, status);
            }
            self.error = Some(status);
            self.active = false;
            self.parsing = false;
            self.snapshot = None;
            self.baseline = None;
            self.sources.clear();
            self.retry_at = if status == SCAN_STALE { now } else { now.saturating_add(300) };
        }
    }

    fn new() -> Result<Self, Status>
    {
        let caller = Caller { owner: crate::toolbox::handles::system_owner()?,
                              kinds: Some(crate::toolbox::handles::file_watch_kinds()?) };
        let volume = files(|manager| {
            manager.volumes
                   .iter()
                   .filter_map(|(_, mounted)| mounted.info())
                   .find(|info| info.startup != 0)
                   .ok_or(NOT_FOUND)
        })?;
        if volume.blessed_folder < ROOT_ID
        {
            return Err(NOT_FOUND);
        }
        let mut parent = FileCall { operation: WATCH_DIRECTORY,
                                    volume: volume.id,
                                    cnid: volume.blessed_folder,
                                    ..Default::default() };
        files(|manager| manager.watch_request(&caller, &mut parent))?;
        Ok(Self { caller,
                  volume: volume.id,
                  source_volume: volume.created,
                  parent_watch: parent.handle,
                  directory: 0,
                  directory_watch: 0,
                  cursor: DirectoryScanCursor::default(),
                  sources: Vec::new(),
                  reading: None,
                  active: false,
                  bytes: 0,
                  retry_at: 0,
                  error: None,
                  baseline: None,
                  snapshot: None,
                  parsing: false })
    }

    fn watch(&self, handle: u32, operation: u32) -> Result<bool, Status>
    {
        if handle == 0
        {
            return Ok(false);
        }
        let mut request = FileCall { operation,
                                     handle,
                                     ..Default::default() };
        files(|manager| manager.watch_request(&self.caller, &mut request))?;
        Ok(request.index != 0)
    }

    fn resolve_directory(&self) -> Result<u32, Status>
    {
        let folder = call(FileCall { operation: FIND_FOLDER,
                                     volume: self.volume,
                                     file_type: *b"font",
                                     ..Default::default() },
                          &mut []);
        match folder
        {
            Ok(reply) => Ok(reply.entry.id),
            Err(NOT_FOUND) => Ok(0),
            Err(status) => Err(status)
        }
    }

    fn begin(&mut self) -> Result<(), Status>
    {
        let directory = self.resolve_directory()?;
        if directory != self.directory
        {
            self.watch(self.directory_watch, DISPOSE_WATCH)?;
            self.directory_watch = 0;
            self.directory = directory;
        }
        if directory != 0 && self.directory_watch == 0
        {
            let mut request = FileCall { operation: WATCH_DIRECTORY,
                                         volume: self.volume,
                                         cnid: directory,
                                         ..Default::default() };
            files(|manager| manager.watch_request(&self.caller, &mut request))?;
            self.directory_watch = request.handle;
        }
        self.cursor = DirectoryScanCursor::default();
        self.sources.clear();
        self.bytes = 0;
        let baseline = crate::kernel::fonts().ok_or(NOT_SUPPORTED)?;
        let snapshot = baseline.begin_installed_snapshot().map_err(|error| error.status())?;
        self.baseline = Some(baseline);
        self.snapshot = Some(snapshot);
        self.parsing = false;
        self.active = true;
        Ok(())
    }

    fn open(&mut self, entry: CatalogInfo) -> Result<(), Status>
    {
        let data_length = usize::try_from(entry.data_length).map_err(|_| NO_MEMORY)?;
        let resource_length = usize::try_from(entry.resource_length).map_err(|_| NO_MEMORY)?;
        if data_length > MAX_FORK_BYTES || resource_length > MAX_FORK_BYTES
        {
            serial!("FONT_SOURCE_REJECTED volume={} cnid={} status={} reason=fork-budget",
                    entry.volume, entry.id, NO_MEMORY);
            return Ok(());
        }
        let length = data_length.checked_add(resource_length).ok_or(NO_MEMORY)?;
        if length > MAX_FORK_BYTES
        {
            serial!("FONT_SOURCE_REJECTED volume={} cnid={} status={} reason=source-budget",
                    entry.volume, entry.id, NO_MEMORY);
            return Ok(());
        }
        if length == 0
        {
            return Ok(());
        }
        if self.sources.len() >= MAX_FILES || length > MAX_SCAN_BYTES - self.bytes
        {
            return Err(NO_MEMORY);
        }
        let mut data = Vec::new();
        let mut resource = Vec::new();
        data.try_reserve_exact(data_length).map_err(|_| NO_MEMORY)?;
        resource.try_reserve_exact(resource_length).map_err(|_| NO_MEMORY)?;
        data.resize(data_length, 0);
        resource.resize(resource_length, 0);
        let fork = if data_length == 0 { RESOURCE_FORK } else { DATA_FORK };
        let reply = call(FileCall { operation: OPEN,
                                    volume: self.volume,
                                    cnid: entry.id,
                                    fork,
                                    ..Default::default() },
                         &mut [])?;
        self.bytes += length;
        self.reading = Some(Reading { source: Source { entry,
                                                       data,
                                                       resource },
                                      handle: reply.handle,
                                      fork,
                                      offset: 0 });
        Ok(())
    }

    fn read(&mut self) -> Result<(), Status>
    {
        let reading = self.reading.as_mut().ok_or(BAD_ABI)?;
        let bytes = if reading.fork == DATA_FORK
        {
            &mut reading.source.data
        }
        else
        {
            &mut reading.source.resource
        };
        let end = reading.offset.saturating_add(READ_BYTES).min(bytes.len());
        let result = call(FileCall { operation: READ,
                                     handle: reading.handle,
                                     ..Default::default() },
                          &mut bytes[reading.offset..end])?;
        if result.transferred != end - reading.offset
        {
            return Err(IO_ERROR);
        }
        reading.offset = end;
        if end != bytes.len()
        {
            return Ok(());
        }
        call(FileCall { operation: CLOSE,
                        handle: reading.handle,
                        ..Default::default() },
             &mut [])?;
        reading.handle = 0;
        if reading.fork == DATA_FORK && !reading.source.resource.is_empty()
        {
            let reply = call(FileCall { operation: OPEN,
                                        volume: self.volume,
                                        cnid: reading.source.entry.id,
                                        fork: RESOURCE_FORK,
                                        ..Default::default() },
                             &mut [])?;
            reading.handle = reply.handle;
            reading.fork = RESOURCE_FORK;
            reading.offset = 0;
            return Ok(());
        }
        self.sources.try_reserve(1).map_err(|_| NO_MEMORY)?;
        let reading = self.reading.take().ok_or(BAD_ABI)?;
        self.sources.push(reading.source);
        Ok(())
    }

    fn finish(&mut self) -> Result<(), Status>
    {
        if !self.parsing
        {
            self.sources.sort_unstable_by_key(|source| core::cmp::Reverse(source.entry.id));
            self.parsing = true;
        }
        if let Some(source) = self.sources.pop()
        {
            let data = if matches!(source.data.get(..4),
                                   Some(b"\0\x01\0\0" | b"true" | b"OTTO" | b"ttcf"))
            {
                InstalledFontData::TrueType(&source.data)
            }
            else if !source.resource.is_empty()
            {
                InstalledFontData::SuitcaseFork(&source.resource)
            }
            else
            {
                serial!("FONT_SOURCE_REJECTED volume={} cnid={} status={} reason=format",
                        source.entry.volume, source.entry.id, NOT_SUPPORTED);
                return Ok(());
            };
            let result = self.snapshot.as_mut().ok_or(BAD_ABI)?.add_source(InstalledFontSource {
                key: FontSourceKey { volume: self.source_volume, cnid: source.entry.id },
                revision: source.entry.modified,
                data
            });
            if let Err(error) = result
            {
                serial!("FONT_SOURCE_REJECTED volume={} cnid={} status={} reason={:?}",
                        source.entry.volume,
                        source.entry.id,
                        error.status(),
                        error.failure);
                if !error.failure.is_source_rejection()
                {
                    return Err(error.status());
                }
            }
            return Ok(());
        }
        if self.cursor.revision != 0
        {
            let revision = files(|manager| Ok(manager.mounted(self.volume)?.catalog_revision))?;
            if revision != self.cursor.revision
            {
                return Err(SCAN_STALE);
            }
        }
        let next = self.snapshot.take().ok_or(BAD_ABI)?.finish().map_err(|error| error.status())?;
        let revision = next.revision();
        let families = next.catalog().len();
        let next = alloc::sync::Arc::try_new(next).map_err(|_| NO_MEMORY)?;
        crate::kernel::publish_fonts(self.baseline.as_ref().ok_or(BAD_ABI)?, next)?;
        self.baseline = None;
        self.active = false;
        self.parsing = false;
        self.error = None;
        self.retry_at = u64::MAX;
        serial!("FONTS_PUBLISHED revision={} families={} volume={} directory={}", revision,
                families, self.volume, self.directory);
        Ok(())
    }
}

pub(crate) fn pump()
{
    let slot = crate::toolbox::handles::InvocationGate::run(|| {
        core::mem::replace(&mut *STATE.lock(), Slot::Busy)
    });
    let Slot::Ready(mut state) = slot
    else
    {
        return;
    };
    let now = crate::platform::ticks();
    if state.refresh.is_none() && now >= state.retry_at
    {
        match Refresh::new()
        {
            Ok(refresh) => state.refresh = Some(refresh),
            Err(status) =>
            {
                if state.error != Some(status)
                {
                    serial!("FONT_REFRESH_FAILED stage=watch-admission status={status}");
                }
                state.error = Some(status);
                state.retry_at = now.saturating_add(300);
            }
        }
    }
    if let Some(refresh) = state.refresh.as_mut()
    {
        refresh.advance();
    }
    crate::toolbox::handles::InvocationGate::run(|| *STATE.lock() = Slot::Ready(state));
}
