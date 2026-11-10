//! File icons from provider volumes, read in the background.
//!
//! A disk answers an icon request at once. A provider (a host folder, a
//! mounted archive) needs several round trips for one icon, so the request is
//! answered "not yet" and remembered here. The System then reads the icon as
//! itself, one file at a time, a step each maintenance turn, puts it in the
//! icon cache, and tells the folder's watchers to look again. A window on a
//! slow volume opens at once and its icons arrive.
//!
//! Most files are documents, whose icon belongs to their creator's
//! application. Those cost one round trip, for the catalogue record, and no
//! fork is opened.
#![forbid(unsafe_code)]
use alloc::{boxed::Box, vec::Vec};

use mac_abi::{files::*, *};
use mac_object_table::shared::InvocationGate as _;

use super::{Manager, ResidentReply, ResidentRequest, SystemLoad,
            application::{Loaded, Selection},
            icon_cache::OWN,
            provider::SystemReader};

/// Icons that may be waiting to be read. More are dropped; whoever wanted
/// them asks again the next time it draws.
const MAX_WANTED: usize = 1024;
/// Maintenance turns between telling watchers, while icons are still being
/// read. Telling them for every icon would have a window read its folder once
/// per file.
const TELL_TURNS: u32 = 96;

/// One icon to read: a file's own (`kind` is `OWN`), or the icon an
/// application declares for a document type.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Wanted
{
    pub volume: u32,
    pub file:   u32,
    pub kind:   [u8; 4]
}

/// What reading came to.
pub(super) enum Outcome
{
    /// Not a program and no custom icon: its creator's application answers.
    Document(CatalogInfo),
    Loaded(Box<Loaded>),
    Failed(Status)
}

/// Whose watchers should look again.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tell
{
    Nobody,
    Folder(u32, u32),
    Everybody
}

enum Stage
{
    Catalog(ResidentRequest),
    Load(SystemLoad)
}
struct Active
{
    wanted: Wanted,
    stage:  Stage
}
struct State
{
    active:  Option<Active>,
    folders: Vec<(u32, u32)>,
    all:     bool,
    turns:   u32
}
enum Slot
{
    Ready(State),
    Busy
}
static STATE: spinning_top::Spinlock<Slot> =
    spinning_top::Spinlock::new(Slot::Ready(State { active:  None,
                                                    folders: Vec::new(),
                                                    all:     false,
                                                    turns:   0 }));
static WANTED: spinning_top::Spinlock<Vec<Wanted>> = spinning_top::Spinlock::new(Vec::new());

fn files<R>(operation: impl FnOnce(&mut Manager) -> Result<R, Status>) -> Result<R, Status>
{
    crate::kernel::with_files(operation).map_err(crate::state::AccessError::status)?
}

/// Remember that an icon is wanted. Called with the File Manager held, so it
/// only notes the request; nothing is read here.
pub(super) fn want(wanted: Wanted)
{
    crate::toolbox::handles::InvocationGate::run(|| {
        let mut queue = WANTED.lock();
        if !queue.contains(&wanted) && queue.len() < MAX_WANTED && queue.try_reserve(1).is_ok()
        {
            queue.push(wanted);
        }
    });
}

fn next() -> Option<Wanted>
{
    crate::toolbox::handles::InvocationGate::run(|| {
        let mut queue = WANTED.lock();
        if queue.is_empty() { None } else { Some(queue.remove(0)) }
    })
}

fn waiting() -> bool
{
    crate::toolbox::handles::InvocationGate::run(|| !WANTED.lock().is_empty())
}

fn working(volume: u32, outstanding: bool)
{
    let _ = files(|manager| {
        manager.system_work(volume, SystemReader::Icons, outstanding);
        Ok(())
    });
}

fn begin(wanted: Wanted) -> Result<Stage, Status>
{
    if wanted.kind != OWN
    {
        return SystemLoad::begin(wanted.volume, wanted.file, Selection::Icons).map(Stage::Load);
    }
    let system = super::provider::runtime::system_subject()?;
    ResidentRequest::file(&system,
                          FileCall { operation: CATALOG,
                                     volume: wanted.volume,
                                     cnid: wanted.file,
                                     ..Default::default() },
                          "").map(Stage::Catalog)
}

impl State
{
    fn settle(&mut self, wanted: Wanted, outcome: Outcome)
    {
        self.active = None;
        working(wanted.volume, false);
        let told = crate::kernel::with_resources_in(mac_abi::resources::SYSTEM, |resident| {
                       files(|manager| manager.provider_icon_settle(wanted, outcome, resident))
                   });
        match told
        {
            Ok(Ok(Tell::Nobody)) => (),
            Ok(Ok(Tell::Everybody)) => self.all = true,
            Ok(Ok(Tell::Folder(volume, folder))) =>
            {
                if !self.folders.contains(&(volume, folder))
                {
                    if self.folders.try_reserve(1).is_ok()
                    {
                        self.folders.push((volume, folder));
                    }
                    else
                    {
                        self.all = true;
                    }
                }
            }
            Ok(Err(status)) | Err(status) =>
            {
                serial!("PROVIDER_ICON_FAILED volume={} file={} status={status}",
                        wanted.volume,
                        wanted.file);
            }
        }
    }

    fn tell(&mut self)
    {
        self.turns = 0;
        if !self.all && self.folders.is_empty()
        {
            return;
        }
        let all = core::mem::take(&mut self.all);
        let folders = core::mem::take(&mut self.folders);
        let _ = files(|manager| {
            if all
            {
                manager.notify_presentation(None)
            }
            else
            {
                folders.iter().try_for_each(|folder| manager.notify_presentation(Some(*folder)))
            }
        });
    }

    fn turn(&mut self)
    {
        self.turns = self.turns.saturating_add(1);
        let Some(active) = self.active.as_mut()
        else
        {
            let Some(wanted) = next()
            else
            {
                // Everything asked for has been read.
                self.tell();
                return;
            };
            if self.turns >= TELL_TURNS
            {
                self.tell();
            }
            // Asked for twice, or answered since it was asked.
            if !matches!(files(|manager| Ok(manager.provider_icon_settled(wanted))), Ok(false))
            {
                return;
            }
            match begin(wanted)
            {
                Ok(stage) =>
                {
                    working(wanted.volume, true);
                    self.active = Some(Active { wanted,
                                                stage });
                }
                // The volume is busy with something of its own. Ask again later.
                Err(SERVICE_BUSY) => want(wanted),
                Err(status) => self.settle(wanted, Outcome::Failed(status))
            }
            return;
        };
        let wanted = active.wanted;
        match &mut active.stage
        {
            Stage::Catalog(request) =>
            {
                let entry = match request.poll()
                {
                    Ok(None) => return,
                    Ok(Some(ResidentReply::File { entry, .. })) => entry,
                    Ok(Some(_)) => return self.settle(wanted, Outcome::Failed(BAD_ABI)),
                    Err(status) => return self.settle(wanted, Outcome::Failed(status))
                };
                let program =
                    matches!(&entry.file_type, b"APPL" | b"INIT" | b"DRVR" | b"FNDR" | b"CLAP");
                let custom = entry.finder_flags() & 0x0400 != 0;
                if entry.kind != FILE
                {
                    return self.settle(wanted, Outcome::Failed(BAD_ARGUMENT));
                }
                if !(program || custom) || entry.resource_length == 0
                {
                    return self.settle(wanted, Outcome::Document(entry));
                }
                match SystemLoad::begin(wanted.volume, wanted.file, Selection::Icons)
                {
                    Ok(load) => active.stage = Stage::Load(load),
                    Err(SERVICE_BUSY) =>
                    {
                        self.active = None;
                        working(wanted.volume, false);
                        want(wanted);
                    }
                    Err(status) => self.settle(wanted, Outcome::Failed(status))
                }
            }
            Stage::Load(load) =>
            {
                match load.poll()
                {
                    Ok(None) => (),
                    Ok(Some(loaded)) => self.settle(wanted, Outcome::Loaded(loaded)),
                    Err(status) => self.settle(wanted, Outcome::Failed(status))
                }
            }
        }
    }
}

/// One maintenance turn: advance the icon being read by one step, or start
/// the next.
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
    if state.active.is_some() || state.all || !state.folders.is_empty() || waiting()
    {
        state.turn();
    }
    crate::toolbox::handles::InvocationGate::run(|| *STATE.lock() = Slot::Ready(state));
}
