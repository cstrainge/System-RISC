//! A recursive native watch is rooted at the export. Directory subscriptions
//! retain physical capabilities, so their handles do not obstruct ancestor moves.
use std::{collections::{BTreeMap, BTreeSet},
          os::windows::io::{FromRawHandle, OwnedHandle},
          sync::{Mutex, OnceLock, Weak}};

use windows_sys::Win32::{Foundation::{ERROR_IO_INCOMPLETE, ERROR_IO_PENDING,
                                      ERROR_NOTIFY_ENUM_DIR, INVALID_HANDLE_VALUE},
                         Storage::FileSystem::*,
                         System::{IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
                                  Threading::CreateEventW}};

use super::*;

pub(crate) struct Changes
{
    state:    Mutex<View>,
    root:     Mutex<Arc<Root>>,
    identity: [u8; 32],
    _pin:     Arc<Dir>,
    parents:  Mutex<Vec<(Weak<Changes>, String)>>
}
#[derive(Default)]
struct View
{
    generation:       u64,
    seen:             bool,
    recheck_location: bool,
    entries:          BTreeMap<Vec<u16>, Option<EntryStamp>>
}
impl View
{
    fn bump(&mut self) -> Result<()>
    {
        self.generation = self.generation.checked_add(1).ok_or(Failure(status::LIMIT))?;
        Ok(())
    }

    fn observe(&mut self, name: Vec<u16>, actual: Option<EntryStamp>) -> Result<bool>
    {
        let changed = self.entries.get(&name) != Some(&actual);
        if self.entries.len() >= 1024 && !self.entries.contains_key(&name)
        {
            return Err(Failure(status::LIMIT));
        }
        self.entries.insert(name, actual);
        Ok(changed)
    }
}
fn registry() -> &'static Mutex<BTreeMap<[u8; 24], Weak<Changes>>>
{
    static SHARED: OnceLock<Mutex<BTreeMap<[u8; 24], Weak<Changes>>>> = OnceLock::new();
    SHARED.get_or_init(|| Mutex::new(BTreeMap::new()))
}
fn directory_identity(directory: &Dir) -> Result<[u8; 32]>
{
    Ok(*object_identity(&pin_directory_file(directory)?)?.ok_or(Failure(status::UNSUPPORTED))?
                                                         .as_bytes())
}
fn relative_path(root: &Dir, child: &Dir) -> Result<Vec<u16>>
{
    let root = dir_path(root)?;
    let child = dir_path(child)?;
    if root == child
    {
        return Ok(Vec::new());
    }
    let prefix = format!("{}\\", root.trim_end_matches('\\'));
    Ok(child.strip_prefix(&prefix).ok_or(Failure(status::STALE))?.encode_utf16().collect())
}
fn private_name(name: &[u16]) -> bool
{
    const PREFIX: &[u16] = &[46, 115, 121, 115, 116, 101, 109, 45, 114, 105, 115, 99, 45];
    name.starts_with(PREFIX)
}
fn under(path: &[u16], parent: &[u16]) -> bool
{
    path == parent || path.starts_with(parent) && path.get(parent.len()) == Some(&92)
}
impl Changes
{
    pub fn shared(directory: Arc<Dir>) -> Result<Arc<Self>>
    {
        let identity = directory_identity(&directory)?;
        let physical: [u8; 24] = identity[..24].try_into().map_err(|_| Failure(status::INVALID))?;
        if let Some(existing) = registry().lock()
                                          .map_err(|_| Failure(status::IO))?
                                          .get(&physical)
                                          .and_then(Weak::upgrade)
        {
            return Ok(existing);
        }
        let root = Root::new(directory.clone())?;
        let created = Arc::new(Self { state: Mutex::new(View::default()),
                                      root: Mutex::new(root.clone()),
                                      identity,
                                      _pin: directory,
                                      parents: Mutex::new(Vec::new()) });
        root.subscribe(&created)?;
        let mut entries = registry().lock().map_err(|_| Failure(status::IO))?;
        if let Some(existing) = entries.get(&physical).and_then(Weak::upgrade)
        {
            return Ok(existing);
        }
        if entries.len() >= 4096
        {
            entries.retain(|_, weak| weak.strong_count() != 0);
        }
        if entries.len() >= 4096
        {
            return Err(Failure(status::LIMIT));
        }
        entries.insert(physical, Arc::downgrade(&created));
        Ok(created)
    }

    fn root(&self) -> Result<Arc<Root>>
    {
        Ok(self.root.lock().map_err(|_| Failure(status::IO))?.clone())
    }

    pub fn identity(&self) -> [u8; 32]
    {
        self.identity
    }

    pub fn generation(&self) -> Result<u64>
    {
        let root = self.root()?;
        root.poll()?;
        let recheck = self.state.lock().map_err(|_| Failure(status::IO))?.recheck_location;
        if recheck
        {
            self.relocate(&root)?;
        }
        let mut state = self.state.lock().map_err(|_| Failure(status::IO))?;
        state.seen = true;
        Ok(state.generation)
    }

    fn relocate(&self, previous: &Arc<Root>) -> Result<()>
    {
        let physical: [u8; 24] =
            self.identity[..24].try_into().map_err(|_| Failure(status::INVALID))?;
        let view = registry().lock()
                             .map_err(|_| Failure(status::IO))?
                             .get(&physical)
                             .and_then(Weak::upgrade)
                             .ok_or(Failure(status::STALE))?;
        if relative_path(&previous.pin, &self._pin).is_ok()
        {
            previous.subscribe(&view)?;
            self.state.lock().map_err(|_| Failure(status::IO))?.recheck_location = false;
            return Ok(());
        }
        // Track the highest known moved ancestor, not a separate named watch
        // for each child. That keeps future ancestor moves unblocked as well.
        let candidates: Vec<_> = previous.routes
                                         .lock()
                                         .map_err(|_| Failure(status::IO))?
                                         .ids
                                         .values()
                                         .filter_map(Weak::upgrade)
                                         .collect();
        let mut anchor = self._pin.clone();
        for child in &candidates
        {
            if !child.state.lock().map_err(|_| Failure(status::IO))?.recheck_location
            {
                continue;
            }
            if relative_path(&previous.pin, &child._pin).is_err()
               && relative_path(&child._pin, &anchor).is_ok()
            {
                anchor = child._pin.clone();
            }
        }
        // Arm the new physical subtree before switching. A tracking-boundary
        // change invalidates old snapshots, including changes made in the gap.
        let replacement = Root::new(anchor.clone())?;
        replacement.absorb(previous, &anchor)?;
        if let Some(anchor) = registry().lock()
                                        .map_err(|_| Failure(status::IO))?
                                        .get(&physical_key(&pin_directory_file(&anchor)?)?)
                                        .and_then(Weak::upgrade)
        {
            anchor.parents.lock().map_err(|_| Failure(status::IO))?.clear();
        }
        Ok(())
    }

    pub fn notify(&self) -> Result<()>
    {
        self.state.lock().map_err(|_| Failure(status::IO))?.bump()
    }

    pub fn observe_parent(self: &Arc<Self>, parent: &Arc<Self>, name: &str) -> Result<()>
    {
        let mut root = parent.root()?;
        let previous = self.root()?;
        if !Arc::ptr_eq(&root, &previous)
        {
            if relative_path(&previous.pin, &root.pin).is_ok()
            {
                previous.absorb(&root, &root.pin)?;
                root = previous;
            }
            else
            {
                root.absorb(&previous, &self._pin)?;
            }
        }
        root.subscribe(self)?;
        *self.root.lock().map_err(|_| Failure(status::IO))? = root;
        // Native directories have one namespace parent. Replace the prior
        // binding when a move is acknowledged; delayed date notifications must
        // reconcile against the new containing folder, not the old name.
        let mut parents = self.parents.lock().map_err(|_| Failure(status::IO))?;
        parents.clear();
        if !private_name(&name.encode_utf16().collect::<Vec<_>>())
        {
            parents.push((Arc::downgrade(parent), name.to_owned()));
        }
        Ok(())
    }

    pub fn observe_entry(&self, name: &str) -> Result<()>
    {
        wire::component(name.as_bytes())?;
        if name.starts_with(".system-risc-")
        {
            return Ok(());
        }
        self.root()?.poll()?;
        let name: Vec<u16> = name.encode_utf16().collect();
        let mut state = self.state.lock().map_err(|_| Failure(status::IO))?;
        state.seen = true;
        if !state.entries.contains_key(&name)
        {
            let actual = entry_stamp(&self._pin, &name)?;
            if actual.is_some()
            {
                state.observe(name, actual)?;
            }
        }
        Ok(())
    }

    pub(crate) fn pinned_directory(&self) -> &Dir
    {
        &self._pin
    }

    pub(crate) fn begin_private_metadata(&self, name: &str) -> Result<EntryStamp>
    {
        self.root()?.poll()?;
        let encoded: Vec<u16> = name.encode_utf16().collect();
        let actual = entry_stamp(&self._pin, &encoded)?.ok_or(Failure(status::STALE))?;
        let mut state = self.state.lock().map_err(|_| Failure(status::IO))?;
        state.seen = true;
        let changed = state.entries.get(&encoded).is_some_and(|prior| *prior != Some(actual));
        state.observe(encoded, Some(actual))?;
        if changed
        {
            state.bump()?;
        }
        Ok(actual)
    }

    pub(crate) fn end_private_metadata(&self, name: &str, before: EntryStamp) -> Result<()>
    {
        let encoded: Vec<u16> = name.encode_utf16().collect();
        let after = entry_stamp(&self._pin, &encoded)?.ok_or(Failure(status::STALE))?;
        if before.identity() != after.identity()
        {
            return Err(Failure(status::STALE));
        }
        let mut state = self.state.lock().map_err(|_| Failure(status::IO))?;
        if before.modified != after.modified
           || before.length != after.length
           || before.links != after.links
           || before.attributes != after.attributes
        {
            state.bump()?;
        }
        state.observe(encoded, Some(after))?;
        Ok(())
    }

    pub fn acknowledge_child(&self, name: &str) -> Result<()>
    {
        wire::component(name.as_bytes())?;
        if name.starts_with(".system-risc-")
        {
            return Ok(());
        }
        self.root()?.poll()?;
        let encoded: Vec<u16> = name.encode_utf16().collect();
        let actual = entry_stamp(&self._pin, &encoded)?;
        let mut state = self.state.lock().map_err(|_| Failure(status::IO))?;
        if state.observe(encoded, actual)?
        {
            state.bump()?;
        }
        Ok(())
    }
}

/// A private operation changes physical directory dates. Acknowledge only its
/// exact observed parent entry after the real directory durability barrier.
pub(crate) fn acknowledge_directory(directory: &Dir) -> Result<()>
{
    let identity = directory_identity(directory)?;
    let physical: [u8; 24] = identity[..24].try_into().map_err(|_| Failure(status::INVALID))?;
    let child =
        registry().lock().map_err(|_| Failure(status::IO))?.get(&physical).and_then(Weak::upgrade);
    let Some(child) = child
    else
    {
        return Ok(());
    };
    let parents = child.parents.lock().map_err(|_| Failure(status::IO))?.clone();
    for (weak, name) in parents
    {
        let Some(parent) = weak.upgrade()
        else
        {
            continue;
        };
        let encoded: Vec<u16> = name.encode_utf16().collect();
        let Some(actual) = entry_stamp(&parent._pin, &encoded)?
        else
        {
            continue;
        };
        if actual.id != physical
        {
            continue;
        }
        let mut state = parent.state.lock().map_err(|_| Failure(status::IO))?;
        if let Some(prior) = state.entries.get_mut(&encoded)
        {
            *prior = Some(actual);
        }
    }
    Ok(())
}

struct Routes
{
    paths:    BTreeMap<Vec<u16>, Weak<Changes>>,
    ids:      BTreeMap<[u8; 24], Weak<Changes>>,
    old_name: Option<Vec<u16>>
}
struct Root
{
    owner:    Weak<Root>,
    pin:      Arc<Dir>,
    io:       Mutex<Watch>,
    dispatch: Mutex<()>,
    routes:   Mutex<Routes>
}
impl Root
{
    fn new(pin: Arc<Dir>) -> Result<Arc<Self>>
    {
        let watch = Watch::new(&pin)?;
        Ok(Arc::new_cyclic(|owner| {
            Self { owner: owner.clone(),
                   io: Mutex::new(watch),
                   pin,
                   dispatch: Mutex::new(()),
                   routes: Mutex::new(Routes { paths:    BTreeMap::new(),
                                               ids:      BTreeMap::new(),
                                               old_name: None }) }
        }))
    }

    fn absorb(self: &Arc<Self>, previous: &Arc<Root>, anchor: &Dir) -> Result<()>
    {
        let views: Vec<_> = previous.routes
                                    .lock()
                                    .map_err(|_| Failure(status::IO))?
                                    .ids
                                    .values()
                                    .filter_map(Weak::upgrade)
                                    .collect();
        for child in views
        {
            if relative_path(anchor, &child._pin).is_err()
            {
                continue;
            }
            self.subscribe(&child)?;
            *child.root.lock().map_err(|_| Failure(status::IO))? = self.clone();
            let mut state = child.state.lock().map_err(|_| Failure(status::IO))?;
            if state.seen
            {
                state.entries.clear();
                state.bump()?;
            }
            state.recheck_location = false;
        }
        Ok(())
    }

    fn subscribe(&self, view: &Arc<Changes>) -> Result<()>
    {
        let path = relative_path(&self.pin, &view._pin)?;
        let mut routes = self.routes.lock().map_err(|_| Failure(status::IO))?;
        if routes.paths.len() >= 4096
        {
            routes.paths.retain(|_, weak| weak.strong_count() != 0);
        }
        let physical: [u8; 24] =
            view.identity[..24].try_into().map_err(|_| Failure(status::INVALID))?;
        if routes.ids.len() >= 4096
        {
            routes.ids.retain(|_, weak| weak.strong_count() != 0);
        }
        if routes.paths.len() >= 4096 && !routes.paths.contains_key(&path)
           || routes.ids.len() >= 4096 && !routes.ids.contains_key(&physical)
        {
            return Err(Failure(status::LIMIT));
        }
        routes.paths.insert(path, Arc::downgrade(view));
        routes.ids.insert(physical, Arc::downgrade(view));
        Ok(())
    }

    fn invalidate_all(&self) -> Result<()>
    {
        let views: Vec<_> = self.routes
                                .lock()
                                .map_err(|_| Failure(status::IO))?
                                .ids
                                .values()
                                .filter_map(Weak::upgrade)
                                .collect();
        for view in views
        {
            if std::ptr::eq(Arc::as_ptr(&view.root()?), self)
            {
                view.notify()?;
            }
        }
        Ok(())
    }

    fn rekey(&self, old: &[u16], new: &[u16], retain_old: bool) -> Result<()>
    {
        let mut routes = self.routes.lock().map_err(|_| Failure(status::IO))?;
        let moved: Vec<_> = routes.paths
                                  .iter()
                                  .filter(|(path, _)| under(path, old))
                                  .map(|(path, weak)| {
                                      let mut updated = new.to_vec();
                                      updated.extend_from_slice(&path[old.len()..]);
                                      (updated, weak.clone())
                                  })
                                  .collect();
        if !retain_old
        {
            for (_, weak) in &moved
            {
                if let Some(view) = weak.upgrade()
                {
                    view.state.lock().map_err(|_| Failure(status::IO))?.recheck_location = false;
                }
            }
        }
        if !retain_old
        {
            routes.paths.retain(|path, _| !under(path, old));
        }
        let added = moved.iter().filter(|(path, _)| !routes.paths.contains_key(path)).count();
        if routes.paths.len() + added > 4096
        {
            return Err(Failure(status::LIMIT));
        }
        routes.paths.extend(moved);
        Ok(())
    }

    fn poll(&self) -> Result<()>
    {
        let _ordered = self.dispatch.lock().map_err(|_| Failure(status::IO))?;
        let Some(batch) = self.io.lock().map_err(|_| Failure(status::IO))?.poll()?
        else
        {
            return Ok(());
        };
        let Some(events) = batch
        else
        {
            return self.invalidate_all();
        };
        let mut queried = BTreeSet::new();
        let mut changed = BTreeMap::new();
        for event in events
        {
            let split = event.name.iter().rposition(|&unit| unit == 92);
            let (parent, name) = split.map_or((&[][..], &event.name[..]), |at| {
                                          (&event.name[..at], &event.name[at + 1..])
                                      });
            if queried.insert(event.name.clone())
            {
                let known = self.routes
                                .lock()
                                .map_err(|_| Failure(status::IO))?
                                .paths
                                .get(parent)
                                .and_then(Weak::upgrade);
                let view = if parent.is_empty()
                {
                    known
                }
                else
                {
                    match entry_stamp(&self.pin, parent)
                    {
                        Ok(Some(actual)) =>
                        {
                            if let Some(prior) = &known
                               && prior.identity[..24] != actual.id
                            {
                                prior.notify()?;
                            }
                            self.routes
                                .lock()
                                .map_err(|_| Failure(status::IO))?
                                .ids
                                .get(&actual.id)
                                .and_then(Weak::upgrade)
                        }
                        Ok(None) =>
                        {
                            if known.is_none()
                            {
                                self.invalidate_all()?;
                            }
                            known
                        } // An earlier path of an already moved directory.
                        Err(_) =>
                        {
                            self.invalidate_all()?;
                            None
                        }
                    }
                };
                if let Some(view) = view
                   && std::ptr::eq(Arc::as_ptr(&view.root()?), self)
                {
                    let actual = entry_stamp(&view._pin, name);
                    let mut state = view.state.lock().map_err(|_| Failure(status::IO))?;
                    let arriving = actual.as_ref().ok().and_then(|value| *value).filter(|stamp| {
                                                                                    stamp.attributes
                                                                         & FILE_ATTRIBUTE_DIRECTORY
                                                                         != 0
                                                                                });
                    let prior = state.entries.get(name).copied().flatten();
                    if matches!(event.action, FILE_ACTION_REMOVED | FILE_ACTION_RENAMED_OLD_NAME)
                       && prior.is_none_or(|stamp| stamp.attributes & FILE_ATTRIBUTE_DIRECTORY != 0)
                    {
                        drop(state);
                        let routes = self.routes.lock().map_err(|_| Failure(status::IO))?;
                        let prefixes: Vec<_> =
                            routes.paths
                                  .iter()
                                  .filter_map(|(path, weak)| {
                                      let child = weak.upgrade()?;
                                      prior.filter(|stamp| child.identity[..24] == stamp.id)
                                           .map(|_| path.clone())
                                  })
                                  .collect();
                        let views: Vec<_> = routes.paths.iter().filter_map(|(path, weak)| {
                            (prior.is_none() || prefixes.iter().any(|prefix| under(path, prefix))).then(|| weak.upgrade()).flatten()
                        }).collect();
                        drop(routes);
                        for child in views
                        {
                            child.state.lock().map_err(|_| Failure(status::IO))?.recheck_location =
                                true;
                        }
                        state = view.state.lock().map_err(|_| Failure(status::IO))?;
                    }
                    let differs = match actual
                    {
                        Ok(actual) => state.observe(name.to_vec(), actual)?,
                        Err(_) => true
                    };
                    if differs
                    {
                        changed.insert(view.identity, view.clone());
                    }
                    drop(state);
                    let arriving_child =
                        if matches!(event.action, FILE_ACTION_ADDED | FILE_ACTION_RENAMED_NEW_NAME)
                           && let Some(actual) = arriving
                        {
                            registry().lock()
                                      .map_err(|_| Failure(status::IO))?
                                      .get(&actual.id)
                                      .and_then(Weak::upgrade)
                        }
                        else
                        {
                            None
                        };
                    if let Some(child) = arriving_child
                    {
                        let current = child.root()?;
                        if !std::ptr::eq(Arc::as_ptr(&current), self)
                           && let Some(root) = self.owner.upgrade()
                        {
                            root.absorb(&current, &child._pin)?;
                        }
                        let name = String::from_utf16(name).map_err(|_| Failure(status::INVALID))?;
                        child.observe_parent(&view, &name)?;
                    }
                }
            }
            if event.action == FILE_ACTION_RENAMED_OLD_NAME
            {
                self.routes.lock().map_err(|_| Failure(status::IO))?.old_name = Some(event.name);
            }
            else if event.action == FILE_ACTION_RENAMED_NEW_NAME
            {
                let old = self.routes.lock().map_err(|_| Failure(status::IO))?.old_name.take();
                if let Some(old) = old
                {
                    self.rekey(&old, &event.name, false)?;
                }
            }
        }
        for view in changed.into_values()
        {
            view.notify()?;
        }
        Ok(())
    }
}
/// Retain old path aliases until the ordered native rename pair is consumed.
/// Events already queued under an old name still reach the same physical view.
pub(crate) fn renamed(from: &Arc<Changes>, old: &str, to: &Arc<Changes>, new: &str) -> Result<()>
{
    let root = from.root()?;
    if !Arc::ptr_eq(&root, &to.root()?)
    {
        return Err(Failure(status::UNSUPPORTED));
    }
    let path = |directory: &Dir, name: &str| -> Result<Vec<u16>> {
        let mut path = relative_path(&root.pin, directory)?;
        if !path.is_empty()
        {
            path.push(92);
        }
        path.extend(name.encode_utf16());
        Ok(path)
    };
    root.rekey(&path(&from._pin, old)?, &path(&to._pin, new)?, true)
}

struct Event
{
    action: u32,
    name:   Vec<u16>
}
fn parse_changes(buffer: &[u32; 1024], bytes: usize) -> Option<Vec<Event>>
{
    const HEADER: usize = std::mem::offset_of!(FILE_NOTIFY_INFORMATION, FileName);
    if bytes == 0 || bytes > 4096
    {
        return None;
    }
    let mut offset = 0usize;
    let mut events = Vec::new();
    loop
    {
        if offset % 4 != 0 || offset.checked_add(HEADER)? > bytes
        {
            return None;
        }
        let at = offset / 4;
        let next = buffer[at] as usize;
        let action = buffer[at + 1];
        let length = buffer[at + 2] as usize;
        if !(FILE_ACTION_ADDED..=FILE_ACTION_RENAMED_NEW_NAME).contains(&action)
           || length == 0
           || length % 2 != 0
           || offset.checked_add(HEADER)?.checked_add(length)? > bytes
        {
            return None;
        }
        let name: Vec<u16> =
            (0..length / 2).map(|i| (buffer[at + HEADER / 4 + i / 2] >> ((i % 2) * 16)) as u16)
                           .collect();
        if String::from_utf16(&name).is_err()
           || name.split(|&unit| unit == 92)
                  .any(|part| part.is_empty() || part == [46] || part == [46, 46])
        {
            return None;
        }
        let base = name.rsplit(|&unit| unit == 92).next()?;
        if !private_name(base)
        {
            events.push(Event { action,
                                name });
        }
        if next == 0
        {
            return Some(events);
        }
        if next < HEADER + length
        {
            return None;
        }
        offset = offset.checked_add(next)?;
    }
}

struct Watch
{
    file:       File,
    event:      OwnedHandle,
    overlapped: Box<OVERLAPPED>,
    buffer:     Box<[u32; 1024]>,
    pending:    bool
}
// SAFETY: the OVERLAPPED and buffer allocations remain stable while pending.
// All host access is serialized by Root::io; Drop cancels and joins kernel I/O.
#[allow(unsafe_code)]
unsafe impl Send for Watch
{
}
impl Watch
{
    #[allow(unsafe_code)]
    fn new(directory: &Dir) -> Result<Self>
    {
        let path: Vec<u16> = OsStr::new(&dir_path(directory)?).encode_wide().chain([0]).collect();
        // SAFETY: the terminated path is derived from a pinned capability. The
        // fresh handle is checked against that capability before it is armed.
        let handle = unsafe {
            CreateFileW(path.as_ptr(),
                        FILE_LIST_DIRECTORY,
                        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                        std::ptr::null(),
                        OPEN_EXISTING,
                        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                        std::ptr::null_mut())
        };
        if handle == INVALID_HANDLE_VALUE
        {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: a fresh owned valid CreateFileW handle.
        let file = unsafe { File::from_raw_handle(handle) };
        if !same_object(&pin_directory_file(directory)?, &file)?
        {
            return Err(Failure(status::STALE));
        }
        // SAFETY: no borrowed security/name pointers; the event is owned here.
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null()
        {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: a fresh valid owned event.
        let event = unsafe { OwnedHandle::from_raw_handle(event) };
        let mut state = Self { file,
                               event,
                               overlapped: Box::new(OVERLAPPED::default()),
                               buffer: Box::new([0; 1024]),
                               pending: false };
        state.arm()?;
        Ok(state)
    }

    #[allow(unsafe_code)]
    fn arm(&mut self) -> Result<()>
    {
        *self.overlapped = OVERLAPPED::default();
        self.overlapped.hEvent = self.event.as_raw_handle();
        // SAFETY: both allocations remain alive until completion or Drop joins
        // cancellation. A recursive request lets child views use unnamed pins.
        let ok = unsafe {
            ReadDirectoryChangesW(self.file.as_raw_handle(),
                                  self.buffer.as_mut_ptr().cast(),
                                  4096,
                                  1,
                                  FILE_NOTIFY_CHANGE_FILE_NAME
                                  | FILE_NOTIFY_CHANGE_DIR_NAME
                                  | FILE_NOTIFY_CHANGE_ATTRIBUTES
                                  | FILE_NOTIFY_CHANGE_SIZE
                                  | FILE_NOTIFY_CHANGE_LAST_WRITE
                                  | FILE_NOTIFY_CHANGE_CREATION
                                  | FILE_NOTIFY_CHANGE_SECURITY,
                                  std::ptr::null_mut(),
                                  &mut *self.overlapped,
                                  None)
        };
        if ok == 0 && io::Error::last_os_error().raw_os_error() != Some(ERROR_IO_PENDING as i32)
        {
            return Err(io::Error::last_os_error().into());
        }
        self.pending = true;
        Ok(())
    }

    #[allow(unsafe_code)]
    fn poll(&mut self) -> Result<Option<Option<Vec<Event>>>>
    {
        if !self.pending
        {
            self.arm()?;
        }
        let mut bytes = 0;
        // SAFETY: the owned pending operation and its storage remain alive.
        // bWait=false bounds each poll to one completed 4 KiB batch.
        let ok = unsafe {
            GetOverlappedResult(self.file.as_raw_handle(), &*self.overlapped, &mut bytes, 0)
        };
        let batch = if ok == 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_IO_INCOMPLETE as i32)
            {
                return Ok(None);
            }
            self.pending = false;
            if error.raw_os_error() != Some(ERROR_NOTIFY_ENUM_DIR as i32)
            {
                return Err(error.into());
            }
            None
        }
        else
        {
            self.pending = false;
            parse_changes(&self.buffer, bytes as usize)
        };
        self.arm()?;
        Ok(Some(batch))
    }
}
impl Drop for Watch
{
    #[allow(unsafe_code)]
    fn drop(&mut self)
    {
        if self.pending
        {
            // SAFETY: cancellation alone does not end the kernel borrow. Join
            // completion before freeing OVERLAPPED, buffer, event or file.
            unsafe {
                CancelIoEx(self.file.as_raw_handle(), &*self.overlapped);
                let mut bytes = 0;
                GetOverlappedResult(self.file.as_raw_handle(), &*self.overlapped, &mut bytes, 1);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EntryStamp
{
    id:         [u8; 24],
    created:    i64,
    modified:   i64,
    changed:    i64,
    attributes: u32,
    length:     i64,
    links:      u32
}
impl EntryStamp
{
    pub(crate) fn identity(&self) -> [u8; 32]
    {
        let mut value = [0; 32];
        value[..24].copy_from_slice(&self.id);
        value[24..].copy_from_slice(&self.created.to_le_bytes());
        value
    }
}
// FILE_STAT_BASIC_INFORMATION (Windows 11 24H2), missing from windows-sys
// 0.61.2. This documented native layout includes ReFS' complete 128-bit ID.
#[repr(C)]
#[derive(Default)]
struct StatBasic
{
    file_id:         i64,
    created:         i64,
    accessed:        i64,
    modified:        i64,
    changed:         i64,
    allocated:       i64,
    length:          i64,
    attributes:      u32,
    reparse:         u32,
    links:           u32,
    device:          u32,
    characteristics: u32,
    reserved:        u32,
    volume:          i64,
    id:              [u8; 16]
}
const _: () = assert!(std::mem::size_of::<StatBasic>() == 104);
#[allow(unsafe_code)]
fn entry_stamp(directory: &Dir, name: &[u16]) -> Result<Option<EntryStamp>>
{
    use windows_sys::{Wdk::{Foundation::OBJECT_ATTRIBUTES,
                            Storage::FileSystem::NtQueryInformationByName},
                      Win32::{Foundation::{OBJ_DONT_REPARSE, RtlNtStatusToDosError,
                                           UNICODE_STRING},
                              System::IO::IO_STATUS_BLOCK}};
    let count = u16::try_from(name.len() * 2).map_err(|_| Failure(status::INVALID))?;
    let text = UNICODE_STRING { Length:        count,
                                MaximumLength: count,
                                Buffer:        name.as_ptr().cast_mut() };
    let attributes = OBJECT_ATTRIBUTES { Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
                                         RootDirectory: directory.as_raw_handle(),
                                         ObjectName: &text,
                                         Attributes: OBJ_DONT_REPARSE,
                                         ..Default::default() };
    let mut completion = IO_STATUS_BLOCK::default();
    let mut information = StatBasic::default();
    // SAFETY: the parent is a pinned directory and name is one checked UTF-16
    // component. Both output buffers are aligned, initialized and alive for
    // this synchronous query. No data handle is opened and no path escapes.
    let status = unsafe {
        NtQueryInformationByName(&attributes,
                                 &mut completion,
                                 (&mut information as *mut StatBasic).cast(),
                                 std::mem::size_of::<StatBasic>() as u32,
                                 77)
    };
    if status < 0
    {
        let error: Failure =
            io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32).into();
        if error.0 == status::NOT_FOUND
        {
            return Ok(None);
        }
        return Err(error);
    }
    if information.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Ok(None);
    }
    if information.id == [0; 16] || information.id == [255; 16]
    {
        return Err(Failure(status::UNSUPPORTED));
    }
    let mut id = [0; 24];
    id[..8].copy_from_slice(&information.volume.to_le_bytes());
    id[8..].copy_from_slice(&information.id);
    Ok(Some(EntryStamp { id,
                         created: information.created,
                         modified: information.modified,
                         changed: information.changed,
                         attributes: information.attributes,
                         length: information.length,
                         links: information.links }))
}

#[cfg(test)]
mod tests
{
    use std::time::{Duration, Instant};

    use super::*;
    struct Temp(std::path::PathBuf);
    impl Temp
    {
        fn new() -> Self
        {
            let mut random = [0; 8];
            getrandom::fill(&mut random).unwrap();
            let path =
                std::env::temp_dir().join(format!("risc-watch-{:x}", u64::from_le_bytes(random)));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn directory(&self, name: &str) -> Arc<Dir>
        {
            Arc::new(pin_directory(Dir::open_ambient_dir(self.0.join(name), cap_std::ambient_authority()).unwrap()).unwrap())
        }
    }
    impl Drop for Temp
    {
        fn drop(&mut self)
        {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn await_change(view: &Changes, previous: u64)
    {
        let deadline = Instant::now() + Duration::from_secs(2);
        while view.generation().unwrap() == previous
        {
            assert!(Instant::now() < deadline, "physical directory notification was lost");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn recursive_subscriptions_follow_native_folder_renames_and_keep_siblings_scoped()
    {
        let temp = Temp::new();
        std::fs::create_dir(temp.0.join("Child")).unwrap();
        std::fs::create_dir(temp.0.join("Sibling")).unwrap();
        std::fs::write(temp.0.join("Child/Data"), b"before").unwrap();
        let parent = Changes::shared(temp.directory("")).unwrap();
        let child = Changes::shared(temp.directory("Child")).unwrap();
        child.observe_parent(&parent, "Child").unwrap();
        parent.observe_entry("Child").unwrap();
        child.observe_entry("Data").unwrap();
        let before_parent = parent.generation().unwrap();
        let before_child = child.generation().unwrap();
        std::fs::rename(temp.0.join("Child"), temp.0.join("Renamed")).unwrap();
        await_change(&parent, before_parent);
        assert_eq!(child.generation().unwrap(),
                   before_child,
                   "moving a folder does not change its children");
        std::fs::write(temp.0.join("Sibling/Unrelated"), b"sibling").unwrap();
        std::fs::write(temp.0.join("Renamed/Data"), b"after!").unwrap();
        await_change(&child, before_child);
        let alias = Changes::shared(temp.directory("Renamed")).unwrap();
        assert!(Arc::ptr_eq(&child, &alias), "a path rename changed the physical watch identity");
        assert_eq!(child.generation().unwrap(), alias.generation().unwrap());
        // Dropping the recursive root while its next I/O is pending exercises
        // cancellation and completion before any buffer storage is released.
    }
    #[test]
    fn private_pin_bookkeeping_preserves_concurrent_host_attribute_changes()
    {
        let temp = Temp::new();
        let path = temp.0.join("Data");
        std::fs::write(&path, b"content").unwrap();
        let view = Changes::shared(temp.directory("")).unwrap();
        view.observe_entry("Data").unwrap();
        let generation = view.generation().unwrap();
        let before = view.begin_private_metadata("Data").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&path, permissions).unwrap();
        view.end_private_metadata("Data", before).unwrap();
        let acknowledged = view.state.lock().unwrap().generation;
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&path, permissions).unwrap();
        assert_ne!(acknowledged, generation,
                   "private link bookkeeping hid an ordinary host attribute edit");
    }
    #[test]
    fn editing_creation_time_does_not_orphan_a_physical_subscription()
    {
        use std::os::windows::fs::{FileTimesExt, OpenOptionsExt};
        let temp = Temp::new();
        std::fs::create_dir(temp.0.join("Child")).unwrap();
        std::fs::write(temp.0.join("Child/Data"), b"before").unwrap();
        let parent = Changes::shared(temp.directory("")).unwrap();
        let child = Changes::shared(temp.directory("Child")).unwrap();
        child.observe_parent(&parent, "Child").unwrap();
        parent.observe_entry("Child").unwrap();
        child.observe_entry("Data").unwrap();
        let before_parent = parent.generation().unwrap();
        let before_child = child.generation().unwrap();
        let directory = std::fs::OpenOptions::new().read(true)
                                                   .write(true)
                                                   .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                                                   .open(temp.0.join("Child"))
                                                   .unwrap();
        directory.set_times(std::fs::FileTimes::new().set_created(
            std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000))).unwrap();
        drop(directory);
        await_change(&parent, before_parent);
        assert!(Arc::ptr_eq(&child, &Changes::shared(temp.directory("Child")).unwrap()),
                "editable creation time replaced a live physical watch");
        std::fs::write(temp.0.join("Child/Data"), b"after!").unwrap();
        await_change(&child, before_child);
    }
    #[test]
    fn subscriptions_follow_a_subtree_outside_the_export_and_back()
    {
        let temp = Temp::new();
        let outside = Temp::new();
        std::fs::create_dir_all(temp.0.join("Child/Nested")).unwrap();
        std::fs::write(temp.0.join("Child/Nested/Data"), b"before").unwrap();
        let parent = Changes::shared(temp.directory("")).unwrap();
        let child = Changes::shared(temp.directory("Child")).unwrap();
        let nested = Changes::shared(temp.directory("Child/Nested")).unwrap();
        child.observe_parent(&parent, "Child").unwrap();
        nested.observe_parent(&child, "Nested").unwrap();
        parent.observe_entry("Child").unwrap();
        child.observe_entry("Nested").unwrap();
        nested.observe_entry("Data").unwrap();
        let before_parent = parent.generation().unwrap();
        let before_nested = nested.generation().unwrap();
        std::fs::rename(temp.0.join("Child"), outside.0.join("Moved")).unwrap();
        await_change(&parent, before_parent);
        assert_ne!(nested.generation().unwrap(),
                   before_nested,
                   "leaving the recursive watch must invalidate a possible tracking gap");
        let before = nested.generation().unwrap();
        std::fs::write(outside.0.join("Moved/Nested/Data"), b"outside").unwrap();
        await_change(&nested, before);
        // Descendants share the moved ancestor's watch and must not leave a
        // named child handle that prevents the ancestor moving back.
        std::fs::rename(outside.0.join("Moved"), temp.0.join("Returned")).unwrap();
        await_change(&parent, before_parent + 1);
        let before = nested.generation().unwrap();
        std::fs::write(temp.0.join("Returned/Nested/Data"), b"returned").unwrap();
        await_change(&nested, before);
        assert!(Arc::ptr_eq(&child.root().unwrap(), &parent.root().unwrap()));
        assert!(Arc::ptr_eq(&nested.root().unwrap(), &parent.root().unwrap()));
    }
    #[test]
    fn overflow_malformed_utf16_and_actions_invalidate_while_private_names_are_filtered()
    {
        let mut buffer = [0u32; 1024];
        let name = ".system-risc-stage";
        const HEADER: usize = std::mem::offset_of!(FILE_NOTIFY_INFORMATION, FileName);
        buffer[1] = FILE_ACTION_ADDED;
        buffer[2] = (name.len() * 2) as u32;
        for (i, unit) in name.encode_utf16().enumerate()
        {
            buffer[HEADER / 4 + i / 2] |= u32::from(unit) << ((i % 2) * 16);
        }
        assert!(parse_changes(&buffer, HEADER + name.len() * 2).unwrap().is_empty());
        assert!(parse_changes(&buffer, 4097).is_none());
        assert!(parse_changes(&buffer, HEADER - 1).is_none());
        buffer[1] = 0;
        assert!(parse_changes(&buffer, HEADER + name.len() * 2).is_none());
        buffer[1] = FILE_ACTION_MODIFIED;
        buffer[2] = 2;
        buffer[HEADER / 4] = 0xd800;
        assert!(parse_changes(&buffer, HEADER + 2).is_none(), "an unpaired surrogate was accepted");
        buffer[2] = 3;
        assert!(parse_changes(&buffer, HEADER + 3).is_none());
    }
}
