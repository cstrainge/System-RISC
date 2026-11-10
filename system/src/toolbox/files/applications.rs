//! Which applications are on each mounted volume, so the System can say which
//! one owns a creator code.
//!
//! The index is in memory only. Each block volume is walked once after it
//! mounts and again after its catalogue has changed and then gone quiet; the
//! walk advances a bounded number of scan steps per maintenance turn and reads
//! catalogue records only, never a fork. Until a walk finishes, the previous
//! answer for that volume stands. Unmounting drops the volume's entries.
#![forbid(unsafe_code)]
use alloc::{sync::Arc, vec::Vec};

use mac_abi::{files::*, *};
use mac_object_table::shared::InvocationGate as _;

use super::{Manager, Mounted};

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
    applications: Vec<Application>,
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
static PUBLISHED: spinning_top::Spinlock<Option<Arc<Vec<Application>>>> =
    spinning_top::Spinlock::new(None);

fn files<R>(operation: impl FnOnce(&mut Manager) -> Result<R, Status>) -> Result<R, Status>
{
    crate::kernel::with_files(operation).map_err(crate::state::AccessError::status)?
}

/// The newest application carrying `creator`, across every indexed volume.
/// Newest is by modification date, then creation date; the classic Finder's
/// rule for two copies of one application.
#[allow(dead_code)]
pub(crate) fn owner(creator: [u8; 4]) -> Option<Application>
{
    let published =
        crate::toolbox::handles::InvocationGate::run(|| PUBLISHED.lock().as_ref().cloned())?;
    published.iter()
             .filter(|application| application.creator == creator)
             .max_by_key(|application| {
                 (application.modified, application.created, application.volume, application.cnid)
             })
             .copied()
}

/// Every indexed application, for inspection.
#[allow(dead_code)]
pub(crate) fn all() -> Option<Arc<Vec<Application>>>
{
    crate::toolbox::handles::InvocationGate::run(|| PUBLISHED.lock().as_ref().cloned())
}

fn publish(state: &State)
{
    let count = state.volumes.iter().map(|volume| volume.applications.len()).sum();
    let mut all = Vec::new();
    if all.try_reserve_exact(count).is_err()
    {
        serial!("APPLICATION_INDEX_PUBLISH_FAILED applications={count} status={NO_MEMORY}");
        return;
    }
    for volume in &state.volumes
    {
        all.extend_from_slice(&volume.applications);
    }
    let all = Arc::new(all);
    crate::toolbox::handles::InvocationGate::run(|| *PUBLISHED.lock() = Some(all));
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
                return false;
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
                    self.applications = walk.found;
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
