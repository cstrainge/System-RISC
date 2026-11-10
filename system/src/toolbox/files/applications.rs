//! Which applications are on each mounted volume, so the System can say which
//! one owns a creator code.
//!
//! The index is in memory only. Each block volume is walked once after it
//! mounts and again after its catalogue has changed and then gone quiet; the
//! walk advances a bounded number of scan steps per maintenance turn and reads
//! catalogue records only, never a fork. Until a walk finishes, the previous
//! answer for that volume stands. Unmounting drops the volume's entries.
//!
//! After a walk, each application's declared document types are read from its
//! `open` resources, one application per maintenance turn, with the same
//! bounded read a document open uses. Those declarations are its claims: with
//! them the System can say which application opens a document.
#![forbid(unsafe_code)]
use alloc::{sync::Arc, vec::Vec};

use mac_abi::{files::*, *};
use mac_object_table::shared::InvocationGate as _;

use super::{Manager, Mounted, application::Selection};

/// Applications remembered per volume. A volume with more is indexed up to
/// this many and the walk's report says so.
const MAX_APPLICATIONS: usize = 4096;
/// Directories that may be waiting to be read at once during one walk.
const MAX_PENDING_DIRECTORIES: usize = 65_536;
/// Catalogue scan steps taken per maintenance turn.
const STEPS_PER_TURN: usize = 4;
/// Maintenance turns a changed catalogue must stay unchanged before it is
/// walked again, so a burst of writes costs one walk and not one per write.
const SETTLE_TURNS: u32 = 64;
const APPLICATION: [u8; 4] = *b"APPL";

/// One application file, as its catalogue record described it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Application
{
    pub volume:   u32,
    pub cnid:     u32,
    pub parent:   u32,
    pub creator:  [u8; 4],
    pub created:  u64,
    pub modified: u64
}

/// An indexed application and the document types it declares it can open.
#[derive(Clone)]
struct Indexed
{
    application: Application,
    types:       Vec<[u8; 4]>,
    /// Whether `types` has been read. An application with no declaration is
    /// probed and empty.
    probed:      bool
}

struct Published
{
    /// Every mounted volume has been walked and every application probed at
    /// least once. Until then a missing answer may only mean "not yet".
    complete: bool,
    entries:  Vec<Indexed>
}

struct Walk
{
    revision:    u64,
    pending:     Vec<u32>,
    directory:   u32,
    cursor:      DirectoryScanCursor,
    found:       Vec<Application>,
    started:     u64,
    steps:       u64,
    directories: u32,
    truncated:   bool
}

struct Volume
{
    id:           u32,
    /// Catalogue revision the published entries were read at; zero before the
    /// first walk finishes.
    built:        u64,
    /// The last revision observed, and how many turns it has stayed the same.
    seen:         u64,
    quiet:        u32,
    /// A revision whose walk failed for a reason that waiting will not cure.
    refused:      u64,
    applications: Vec<Indexed>,
    walk:         Option<Walk>
}

struct State
{
    volumes: Vec<Volume>,
    next:    usize
}
enum Slot
{
    Ready(State),
    Busy
}
static STATE: spinning_top::Spinlock<Slot> =
    spinning_top::Spinlock::new(Slot::Ready(State { volumes: Vec::new(),
                                                    next:    0 }));
/// Every volume's finished entries, replaced whole when any volume's change.
static PUBLISHED: spinning_top::Spinlock<Option<Arc<Published>>> =
    spinning_top::Spinlock::new(None);

fn files<R>(operation: impl FnOnce(&mut Manager) -> Result<R, Status>) -> Result<R, Status>
{
    crate::kernel::with_files(operation).map_err(crate::state::AccessError::status)?
}

fn published() -> Option<Arc<Published>>
{
    crate::toolbox::handles::InvocationGate::run(|| PUBLISHED.lock().as_ref().cloned())
}

fn newest(application: &Application) -> (u64, u64, u32, u32)
{
    (application.modified, application.created, application.volume, application.cnid)
}

/// The newest application carrying `creator`, across every indexed volume.
/// Newest is by modification date, then creation date; the classic Finder's
/// rule for two copies of one application.
pub(crate) fn owner(creator: [u8; 4]) -> Result<Application, Status>
{
    let published = published().ok_or(SERVICE_BUSY)?;
    published.entries
             .iter()
             .map(|entry| &entry.application)
             .filter(|application| application.creator == creator)
             .max_by_key(|application| newest(application))
             .copied()
             .ok_or(if published.complete { NOT_FOUND } else { SERVICE_BUSY })
}

/// The application that opens a document of this creator and type.
///
/// Among the applications declaring the type: one whose own creator is the
/// document's wins; then the one declaring the fewest types, so a general
/// browser does not take a document from its editor; then the newest.
/// `SERVICE_BUSY` means the index is still being built and has no answer yet.
pub(crate) fn handler(creator: [u8; 4], file_type: [u8; 4]) -> Result<Application, Status>
{
    let published = published().ok_or(SERVICE_BUSY)?;
    published.entries
             .iter()
             .filter(|entry| entry.types.binary_search(&file_type).is_ok())
             .min_by_key(|entry| {
                 (entry.application.creator != creator,
                  entry.types.len(),
                  core::cmp::Reverse(newest(&entry.application)))
             })
             .map(|entry| entry.application)
             .ok_or(if published.complete { NOT_FOUND } else { SERVICE_BUSY })
}

fn publish(state: &State)
{
    let count = state.volumes.iter().map(|volume| volume.applications.len()).sum();
    let mut entries = Vec::new();
    if entries.try_reserve_exact(count).is_err()
    {
        serial!("APPLICATION_INDEX_PUBLISH_FAILED applications={count} status={NO_MEMORY}");
        return;
    }
    for volume in &state.volumes
    {
        for entry in &volume.applications
        {
            let mut types = Vec::new();
            if types.try_reserve_exact(entry.types.len()).is_err()
            {
                serial!("APPLICATION_INDEX_PUBLISH_FAILED applications={count} \
                         status={NO_MEMORY}");
                return;
            }
            types.extend_from_slice(&entry.types);
            entries.push(Indexed { application: entry.application,
                                   types,
                                   probed: entry.probed });
        }
    }
    let complete = state.volumes.iter().all(|volume| {
                                           volume.built != 0
                                           && volume.applications.iter().all(|entry| entry.probed)
                                       });
    let Ok(published) = Arc::try_new(Published { complete,
                                                 entries })
    else
    {
        serial!("APPLICATION_INDEX_PUBLISH_FAILED applications={count} status={NO_MEMORY}");
        return;
    };
    crate::toolbox::handles::InvocationGate::run(|| *PUBLISHED.lock() = Some(published));
}

/// Block volumes that can be scanned right now, with their catalogue revision.
fn mounted() -> Result<Vec<(u32, u64)>, Status>
{
    files(|manager| {
        let mut list = Vec::new();
        for (id, mounted) in manager.volumes.iter()
        {
            if let Mounted::Block(block) = mounted
            {
                if block.scanner.is_some() && !block.poisoned
                {
                    list.try_reserve(1).map_err(|_| NO_MEMORY)?;
                    list.push((id, block.catalog_revision));
                }
            }
        }
        Ok(list)
    })
}

impl Walk
{
    fn new(revision: u64) -> Result<Self, Status>
    {
        let mut pending = Vec::new();
        pending.try_reserve(1).map_err(|_| NO_MEMORY)?;
        pending.push(ROOT_ID);
        Ok(Self { revision,
                  pending,
                  directory: 0,
                  cursor: DirectoryScanCursor::default(),
                  found: Vec::new(),
                  started: crate::platform::ticks(),
                  steps: 0,
                  directories: 0,
                  truncated: false })
    }

    /// One scan step. `Ok(true)` when the whole catalogue has been read.
    fn step(&mut self, volume: u32) -> Result<bool, Status>
    {
        if self.directory == 0
        {
            let Some(next) = self.pending.pop()
            else
            {
                return Ok(true);
            };
            self.directory = next;
            self.directories = self.directories.saturating_add(1);
            self.cursor = DirectoryScanCursor::default();
        }
        let mut request = FileScanCall { operation: SCAN_DIRECTORY,
                                         volume,
                                         directory: self.directory,
                                         cursor: self.cursor,
                                         ..Default::default() };
        files(|manager| manager.scan_request(&mut request))?;
        self.cursor = request.cursor;
        self.steps = self.steps.saturating_add(1);
        match request.step
        {
            SCAN_ENTRY =>
            {
                let entry = &request.entry;
                if entry.kind == DIRECTORY
                {
                    // An application in the Trash owns nothing.
                    let name = entry.name.get(..entry.name_length as usize).unwrap_or(&[]);
                    let trash = entry.parent == ROOT_ID && name == b"Trash";
                    if !trash
                    {
                        if self.pending.len() >= MAX_PENDING_DIRECTORIES
                           || self.pending.try_reserve(1).is_err()
                        {
                            self.truncated = true;
                        }
                        else
                        {
                            self.pending.push(entry.id);
                        }
                    }
                }
                else if entry.file_type == APPLICATION
                {
                    if self.found.len() >= MAX_APPLICATIONS || self.found.try_reserve(1).is_err()
                    {
                        self.truncated = true;
                    }
                    else
                    {
                        self.found.push(Application { volume,
                                                      cnid: entry.id,
                                                      parent: entry.parent,
                                                      creator: entry.creator,
                                                      created: entry.created,
                                                      modified: entry.modified });
                    }
                }
            }
            SCAN_PENDING => (),
            SCAN_DONE => self.directory = 0,
            _ => return Err(BAD_ABI)
        }
        Ok(false)
    }
}

impl Volume
{
    /// Advance this volume by one maintenance turn. Returns whether its
    /// published entries changed.
    fn advance(&mut self, revision: u64) -> bool
    {
        if self.seen != revision
        {
            self.seen = revision;
            self.quiet = 0;
        }
        else
        {
            self.quiet = self.quiet.saturating_add(1);
        }
        if self.walk.is_none()
        {
            let due = self.built != revision
                      && self.refused != revision
                      && (self.built == 0 || self.quiet >= SETTLE_TURNS);
            if !due
            {
                return self.probe();
            }
            match Walk::new(revision)
            {
                Ok(walk) => self.walk = Some(walk),
                Err(status) =>
                {
                    serial!("APPLICATION_INDEX_FAILED volume={} stage=begin status={status}",
                            self.id);
                    self.refused = revision;
                    return false;
                }
            }
        }
        for _ in 0..STEPS_PER_TURN
        {
            let Some(walk) = self.walk.as_mut()
            else
            {
                return false;
            };
            match walk.step(self.id)
            {
                Ok(false) => (),
                Ok(true) =>
                {
                    let Some(walk) = self.walk.take()
                    else
                    {
                        return false;
                    };
                    let elapsed = crate::platform::ticks().saturating_sub(walk.started);
                    let microseconds =
                        crate::machine::monitor::ticks_to_nanoseconds(elapsed).map(|n| n / 1000)
                                                                              .unwrap_or(0);
                    serial!("APPLICATION_INDEX_READY volume={} revision={} applications={} \
                             directories={} steps={} microseconds={} truncated={}",
                            self.id,
                            walk.revision,
                            walk.found.len(),
                            walk.directories,
                            walk.steps,
                            microseconds,
                            walk.truncated);
                    let mut applications = Vec::new();
                    if applications.try_reserve_exact(walk.found.len()).is_err()
                    {
                        serial!("APPLICATION_INDEX_FAILED volume={} stage=finish status={}",
                                self.id,
                                NO_MEMORY);
                        self.refused = revision;
                        return false;
                    }
                    for application in walk.found
                    {
                        // An application that has not changed keeps the claims
                        // already read from it.
                        let kept = self.applications
                                       .iter_mut()
                                       .find(|old| old.probed && old.application == application)
                                       .map(|old| core::mem::take(&mut old.types));
                        applications.push(Indexed { application,
                                                    probed: kept.is_some(),
                                                    types: kept.unwrap_or_default() });
                    }
                    self.applications = applications;
                    self.built = walk.revision;
                    return true;
                }
                // The catalogue moved under the walk, or the volume is busy with
                // an operation of its own. Neither is a failure: start again once
                // it has been quiet.
                Err(SCAN_STALE | SERVICE_BUSY) =>
                {
                    self.walk = None;
                    self.quiet = 0;
                    return false;
                }
                Err(status) =>
                {
                    serial!("APPLICATION_INDEX_FAILED volume={} stage=scan status={status}",
                            self.id);
                    self.walk = None;
                    self.refused = revision;
                    return false;
                }
            }
        }
        false
    }
}

impl Volume
{
    /// Read one application's declared document types. Returns whether that
    /// was the last one outstanding, so the finished claims can be published.
    fn probe(&mut self) -> bool
    {
        let volume = self.id;
        let Some(entry) = self.applications.iter_mut().find(|entry| !entry.probed)
        else
        {
            return false;
        };
        let cnid = entry.application.cnid;
        let read = files(|manager| {
            manager.application_resources_selected(volume, cnid, Selection::OpenTypes)
        });
        let types = match read
        {
            Ok((catalog, resources)) =>
            {
                match crate::toolbox::processes::documents::declarations(catalog.creator,
                                                                         &resources)
                {
                    Ok(types) => types,
                    // No declaration: the application opens no documents.
                    Err(NOT_SUPPORTED) => Vec::new(),
                    Err(status) =>
                    {
                        serial!("APPLICATION_CLAIMS_REFUSED volume={volume} cnid={cnid} \
                                 status={status}");
                        Vec::new()
                    }
                }
            }
            // Busy now; this application is tried again on a later turn.
            Err(SERVICE_BUSY) => return false,
            // No resource fork, or no longer an application.
            Err(NOT_FOUND | BAD_ARGUMENT) => Vec::new(),
            Err(status) =>
            {
                serial!("APPLICATION_CLAIMS_REFUSED volume={volume} cnid={cnid} status={status}");
                Vec::new()
            }
        };
        if !types.is_empty()
        {
            let creator = entry.application.creator;
            serial!("APPLICATION_CLAIMS volume={volume} cnid={cnid} creator={} types={}",
                    core::str::from_utf8(&creator).unwrap_or("????"),
                    types.len());
        }
        entry.types = types;
        entry.probed = true;
        if self.applications.iter().any(|entry| !entry.probed)
        {
            return false;
        }
        let declaring = self.applications.iter().filter(|entry| !entry.types.is_empty()).count();
        let types: usize = self.applications.iter().map(|entry| entry.types.len()).sum();
        serial!("APPLICATION_CLAIMS_READY volume={} applications={} declaring={} types={}",
                volume,
                self.applications.len(),
                declaring,
                types);
        true
    }
}

/// One maintenance turn: notice mounts and unmounts, then advance one volume.
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
    let mut changed = false;
    if let Ok(mounted) = mounted()
    {
        let before = state.volumes.len();
        state.volumes.retain(|volume| mounted.iter().any(|(id, _)| *id == volume.id));
        changed |= state.volumes.len() != before;
        for (id, _) in &mounted
        {
            if !state.volumes.iter().any(|volume| volume.id == *id)
               && state.volumes.try_reserve(1).is_ok()
            {
                state.volumes.push(Volume { id:           *id,
                                            built:        0,
                                            seen:         0,
                                            quiet:        0,
                                            refused:      0,
                                            applications: Vec::new(),
                                            walk:         None });
            }
        }
        if !state.volumes.is_empty()
        {
            state.next %= state.volumes.len();
            let at = state.next;
            state.next = state.next.wrapping_add(1);
            let id = state.volumes[at].id;
            if let Some((_, revision)) = mounted.iter().find(|(mounted, _)| *mounted == id)
            {
                changed |= state.volumes[at].advance(*revision);
            }
        }
    }
    if changed
    {
        publish(&state);
    }
    crate::toolbox::handles::InvocationGate::run(|| *STATE.lock() = Slot::Ready(state));
}
