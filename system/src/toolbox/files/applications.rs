//! Which applications are on each mounted volume, so the System can say which
//! one owns a creator code.
//!
//! The index is in memory only. Each volume is walked once after it mounts
//! and again after its catalogue has changed and then gone quiet; the walk
//! advances a bounded number of scan steps per maintenance turn and reads
//! catalogue records only, never a fork. Until a walk finishes, the previous
//! answer for that volume stands. Unmounting drops the volume's entries.
//!
//! A provider volume (a host folder, a mounted archive) is a drive like any
//! other: it is walked and its applications' declarations are read. It
//! answers later, through a job, and the System asks as itself: no application
//! is behind the question.
//!
//! After a walk, each application's declared document types are read from its
//! `open` resources, one application per maintenance turn, with the same
//! bounded read a document open uses. Those declarations are its claims: with
//! them the System can say which application opens a document.
#![forbid(unsafe_code)]
use alloc::{sync::Arc, vec::Vec};

use mac_abi::{files::*, *};
use mac_object_table::shared::InvocationGate as _;

use super::{Manager, Mounted, ResidentReply, ResidentRequest, SystemLoad,
            application::Selection};

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
    volume:      u32,
    revision:    u64,
    pending:     Vec<u32>,
    directory:   u32,
    cursor:      DirectoryScanCursor,
    found:       Vec<Application>,
    started:     u64,
    steps:       u64,
    directories: u32,
    truncated:   bool,
    /// The System's own authority to read a provider volume; none for a block
    /// volume, which the System reads directly.
    system:      Option<crate::drivers::jobs::subjects::Subject>,
    /// The provider scan step that has been asked and not yet answered.
    request:     Option<ResidentRequest>
}

struct Volume
{
    id:           u32,
    provider:     bool,
    /// Catalogue revision the published entries were read at; zero before the
    /// first walk finishes.
    built:        u64,
    /// The last revision observed, and how many turns it has stayed the same.
    seen:         u64,
    quiet:        u32,
    /// A revision whose walk failed for a reason that waiting will not cure.
    refused:      u64,
    applications: Vec<Indexed>,
    walk:         Option<Walk>,
    /// A provider volume's application whose declarations are being read, by
    /// catalogue id, and the read.
    load:         Option<(u32, SystemLoad)>
}

struct State
{
    volumes:   Vec<Volume>,
    next:      usize,
    /// The last complete index. An arrival is measured against it.
    announced: Option<Arc<Published>>
}
enum Slot
{
    Ready(State),
    Busy
}
static STATE: spinning_top::Spinlock<Slot> =
    spinning_top::Spinlock::new(Slot::Ready(State { volumes:   Vec::new(),
                                                    next:      0,
                                                    announced: None }));
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

/// Whether `now` holds an application, or a declared type, `before` lacks.
/// Losses do not count: an icon already shown for a document stays right enough
/// when its application goes away.
fn arrived(before: Option<&Published>, now: &Published) -> bool
{
    let Some(before) = before
    else
    {
        return !now.entries.is_empty();
    };
    now.entries.iter().any(|entry| {
                          let same = |old: &&Indexed| {
                              old.application.volume == entry.application.volume
                              && old.application.cnid == entry.application.cnid
                          };
                          match before.entries.iter().find(same)
                          {
                              Some(old) =>
                              {
                                  entry.types.iter().any(|kind| !old.types.contains(kind))
                              }
                              None => true
                          }
                      })
}

fn publish(state: &mut State)
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
    // Probing answers one application a turn. Announcing each would have every
    // watcher read every folder once per application, so a volume is announced
    // when all of it is known. A slow volume does not hold up a finished one.
    let settled = |volume: &&Volume| {
        volume.built != 0 && volume.applications.iter().all(|entry| entry.probed)
    };
    let mut entries = Vec::new();
    for entry in state.volumes.iter().filter(settled).flat_map(|volume| &volume.applications)
    {
        let mut types = Vec::new();
        if entries.try_reserve(1).is_err() || types.try_reserve_exact(entry.types.len()).is_err()
        {
            return;
        }
        types.extend_from_slice(&entry.types);
        entries.push(Indexed { application: entry.application,
                               types,
                               probed: true });
    }
    let Ok(published) = Arc::try_new(Published { complete: true,
                                                 entries })
    else
    {
        return;
    };
    if arrived(state.announced.as_deref(), &published)
    {
        match files(|manager| manager.applications_arrived())
        {
            Ok(()) => serial!("APPLICATIONS_ARRIVED applications={}", published.entries.len()),
            // The old snapshot stays, so the next publication tries again.
            Err(status) =>
            {
                serial!("APPLICATIONS_ARRIVED_FAILED status={status}");
                return;
            }
        }
    }
    state.announced = Some(published);
}

/// Volumes that can be scanned right now, with their catalogue revision and
/// whether a provider serves them.
fn mounted() -> Result<Vec<(u32, u64, bool)>, Status>
{
    files(|manager| {
        let mut list = Vec::new();
        for (id, mounted) in manager.volumes.iter()
        {
            let found = match mounted
            {
                Mounted::Block(block) if block.scanner.is_some() && !block.poisoned =>
                {
                    Some((block.catalog_revision, false))
                }
                Mounted::Provider(provider)
                    if matches!(provider.phase, super::provider::Phase::Live) =>
                {
                    Some((provider.revision, true))
                }
                _ => None
            };
            if let Some((revision, provider)) = found
            {
                list.try_reserve(1).map_err(|_| NO_MEMORY)?;
                list.push((id, revision, provider));
            }
        }
        Ok(list)
    })
}

impl Walk
{
    fn new(volume: u32, revision: u64, provider: bool) -> Result<Self, Status>
    {
        let system = if provider
        {
            Some(super::provider::runtime::system_subject()?)
        }
        else
        {
            None
        };
        let mut pending = Vec::new();
        pending.try_reserve(1).map_err(|_| NO_MEMORY)?;
        pending.push(ROOT_ID);
        Ok(Self { volume,
                  revision,
                  pending,
                  directory: 0,
                  cursor: DirectoryScanCursor::default(),
                  found: Vec::new(),
                  started: crate::platform::ticks(),
                  steps: 0,
                  directories: 0,
                  truncated: false,
                  system,
                  request: None })
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
        let request = match &self.system
        {
            None =>
            {
                files(|manager| manager.scan_request(&mut request))?;
                request
            }
            Some(system) =>
            {
                let asked = match self.request.as_mut()
                {
                    Some(asked) => asked,
                    None =>
                    {
                        let asked = ResidentRequest::scan(system, request)?;
                        if asked.pending()
                        {
                            let _ = files(|manager| {
                                manager.index_read(volume, true);
                                Ok(())
                            });
                        }
                        self.request.insert(asked)
                    }
                };
                // A refused poll keeps its request, and dropping the request
                // hands its cleanup to the System, so every way out below
                // lets go of it.
                let answer = asked.poll();
                if !matches!(answer, Ok(None))
                {
                    self.request = None;
                    let _ = files(|manager| {
                        manager.index_read(volume, false);
                        Ok(())
                    });
                }
                match answer?
                {
                    None => return Ok(false),
                    Some(ResidentReply::Scan(reply)) => reply,
                    Some(_) => return Err(BAD_ABI)
                }
            }
        };
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

impl Drop for Walk
{
    /// A walk given up with a step still outstanding no longer vouches for it.
    /// The abandoned step then counts as an ordinary read until the provider
    /// has acknowledged it.
    fn drop(&mut self)
    {
        if self.request.is_some()
        {
            let volume = self.volume;
            let _ = files(|manager| {
                manager.index_read(volume, false);
                Ok(())
            });
        }
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
            match Walk::new(self.id, revision, self.provider)
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
                    let Some(mut walk) = self.walk.take()
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
                    for application in core::mem::take(&mut walk.found)
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
        let read = if self.provider
        {
            if self.load.as_ref().is_some_and(|(reading, _)| *reading != cnid)
            {
                self.load = None;
            }
            let mut refused = None;
            if self.load.is_none()
            {
                match SystemLoad::begin(volume, cnid, Selection::OpenTypes)
                {
                    Ok(load) => self.load = Some((cnid, load)),
                    Err(status) => refused = Some(status)
                }
            }
            let polled = match (self.load.as_mut(), refused)
            {
                (_, Some(status)) => Err(status),
                (Some((_, load)), None) => load.poll(),
                (None, None) => Err(BAD_ABI)
            };
            match polled
            {
                // Still reading; the next turn asks again.
                Ok(None) => return false,
                Ok(Some(loaded)) =>
                {
                    self.load = None;
                    let loaded = *loaded;
                    Ok((loaded.catalog, loaded.resources))
                }
                Err(status) =>
                {
                    self.load = None;
                    Err(status)
                }
            }
        }
        else
        {
            files(|manager| {
                manager.application_resources_selected(volume, cnid, Selection::OpenTypes)
            })
        };
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
        state.volumes.retain(|volume| mounted.iter().any(|(id, ..)| *id == volume.id));
        changed |= state.volumes.len() != before;
        for (id, _, provider) in &mounted
        {
            if !state.volumes.iter().any(|volume| volume.id == *id)
               && state.volumes.try_reserve(1).is_ok()
            {
                state.volumes.push(Volume { id:           *id,
                                            provider:     *provider,
                                            built:        0,
                                            seen:         0,
                                            quiet:        0,
                                            refused:      0,
                                            applications: Vec::new(),
                                            walk:         None,
                                            load:         None });
            }
        }
        if !state.volumes.is_empty()
        {
            state.next %= state.volumes.len();
            let at = state.next;
            state.next = state.next.wrapping_add(1);
            let id = state.volumes[at].id;
            if let Some((_, revision, _)) = mounted.iter().find(|(mounted, ..)| *mounted == id)
            {
                changed |= state.volumes[at].advance(*revision);
            }
        }
    }
    if changed
    {
        publish(&mut state);
    }
    crate::toolbox::handles::InvocationGate::run(|| *STATE.lock() = Slot::Ready(state));
}
