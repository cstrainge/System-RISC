//! Nonblocking host notifications include child content/metadata edits, which
//! directory mtime alone cannot see. Reads are bounded to one 4 KiB event batch.
//! On macOS the watch is a kqueue directory descriptor: it carries no file
//! name, so every event invalidates (over-invalidation is safe, and the
//! drain stays bounded). Windows uses a pinned overlapped directory handle.
#![deny(unsafe_code)]
#[cfg(target_os = "macos")]
use std::{collections::BTreeMap,
          fs::File,
          sync::{Mutex, OnceLock, Weak}};
#[cfg(target_os = "linux")]
use std::{collections::BTreeMap,
          mem::MaybeUninit,
          os::fd::{AsRawFd, OwnedFd},
          sync::{Mutex, OnceLock, Weak}};

use super::*;
#[cfg(target_os = "linux")]
pub(crate) struct Changes
{
    fd:         OwnedFd,
    generation: Mutex<u64>,
    identity:   [u8; 32],
    _pin:       Arc<Dir>
}
#[cfg(target_os = "linux")]
impl Changes
{
    pub fn shared(directory: Arc<Dir>) -> Result<Arc<Self>>
    {
        use std::os::unix::fs::MetadataExt;
        static SHARED: OnceLock<Mutex<BTreeMap<[u8; 32], Weak<Changes>>>> = OnceLock::new();
        let shared = SHARED.get_or_init(|| Mutex::new(BTreeMap::new()));
        let file = pin_directory_file(&directory)?;
        let metadata = file.metadata()?;
        let identity = if let Some(identity) = object_identity(&file)?
        {
            *identity.as_bytes()
        }
        else
        {
            let mut key = [0; 32];
            key[..8].copy_from_slice(&metadata.dev().to_le_bytes());
            key[8..16].copy_from_slice(&metadata.ino().to_le_bytes());
            key
        };
        if let Some(existing) =
            shared.lock().map_err(|_| Failure(status::IO))?.get(&identity).and_then(Weak::upgrade)
        {
            return Ok(existing);
        }
        // Construct the watch outside the registry lock. The retained directory
        // pin prevents inode reuse while any alias still holds this cache entry.
        let created = Arc::new(Self::new(directory, identity)?);
        let mut registry = shared.lock().map_err(|_| Failure(status::IO))?;
        if let Some(existing) = registry.get(&identity).and_then(Weak::upgrade)
        {
            return Ok(existing);
        }
        if registry.len() >= 4096
        {
            registry.retain(|_, weak| weak.strong_count() != 0);
        }
        if registry.len() >= 4096
        {
            return Err(Failure(status::LIMIT));
        }
        registry.insert(identity, Arc::downgrade(&created));
        Ok(created)
    }

    fn new(directory: Arc<Dir>, identity: [u8; 32]) -> Result<Self>
    {
        use rustix::fs::inotify::{self, CreateFlags, WatchFlags as W};
        let fd =
            inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK).map_err(io::Error::from)?;
        // This path names an already checked, pinned capability. No guest path
        // is resolved through ambient authority or follows an untrusted symlink.
        let path = format!("/proc/self/fd/{}", directory.as_raw_fd());
        inotify::add_watch(&fd,
                           path,
                           W::ONLYDIR
                           | W::ATTRIB
                           | W::CLOSE_WRITE
                           | W::MODIFY
                           | W::CREATE
                           | W::DELETE
                           | W::MOVED_FROM
                           | W::MOVED_TO
                           // Moving this directory does not change its
                           // children. The old/new parents receive MOVED_*;
                           // invalidating this snapshot would break a freshly
                           // published recursive tree at its first read.
                           | W::DELETE_SELF).map_err(io::Error::from)?;
        Ok(Self { fd,
                  generation: Mutex::new(0),
                  identity,
                  _pin: directory })
    }

    pub fn identity(&self) -> [u8; 32]
    {
        self.identity
    }

    pub fn notify(&self) -> Result<()>
    {
        let mut generation = self.generation.lock().map_err(|_| Failure(status::IO))?;
        *generation = generation.checked_add(1).ok_or(Failure(status::LIMIT))?;
        Ok(())
    }

    pub fn generation(&self) -> Result<u64>
    {
        use rustix::fs::inotify;
        let mut generation = self.generation.lock().map_err(|_| Failure(status::IO))?;
        let mut bytes = [MaybeUninit::uninit(); 4096];
        let mut reader = inotify::Reader::new(&self.fd, &mut bytes);
        let mut changed = false;
        loop
        {
            match reader.next()
            {
                Ok(event) =>
                {
                    // Private records are not directory contents. Their logical
                    // changes explicitly notify the affected folder once.
                    // Nameless events (including overflow) always invalidate.
                    if event.file_name()
                            .is_none_or(|name| !name.to_bytes().starts_with(b".system-risc-"))
                    {
                        changed = true;
                    }
                }
                Err(rustix::io::Errno::AGAIN) => break,
                Err(error) => return Err(io::Error::from(error).into())
            }
            if reader.is_buffer_empty()
            {
                break;
            }
        }
        if changed
        {
            *generation = generation.checked_add(1).ok_or(Failure(status::LIMIT))?;
        }
        Ok(*generation)
    }
}
#[cfg(target_os = "macos")]
pub(crate) struct Changes
{
    watcher:    kqueue::Watcher,
    _watch:     File,
    generation: Mutex<u64>,
    identity:   [u8; 32],
    _pin:       Arc<Dir>
}
#[cfg(target_os = "macos")]
impl Changes
{
    pub fn shared(directory: Arc<Dir>) -> Result<Arc<Self>>
    {
        use std::os::unix::fs::MetadataExt;
        static SHARED: OnceLock<Mutex<BTreeMap<[u8; 32], Weak<Changes>>>> = OnceLock::new();
        let shared = SHARED.get_or_init(|| Mutex::new(BTreeMap::new()));
        let file = pin_directory_file(&directory)?;
        let metadata = file.metadata()?;
        let identity = if let Some(identity) = object_identity(&file)?
        {
            *identity.as_bytes()
        }
        else
        {
            let mut key = [0; 32];
            key[..8].copy_from_slice(&metadata.dev().to_le_bytes());
            key[8..16].copy_from_slice(&metadata.ino().to_le_bytes());
            key
        };
        if let Some(existing) =
            shared.lock().map_err(|_| Failure(status::IO))?.get(&identity).and_then(Weak::upgrade)
        {
            return Ok(existing);
        }
        // Construct the watch outside the registry lock. The retained directory
        // pin prevents inode reuse while any alias still holds this cache entry.
        let created = Arc::new(Self::new(directory, identity)?);
        let mut registry = shared.lock().map_err(|_| Failure(status::IO))?;
        if let Some(existing) = registry.get(&identity).and_then(Weak::upgrade)
        {
            return Ok(existing);
        }
        if registry.len() >= 4096
        {
            registry.retain(|_, weak| weak.strong_count() != 0);
        }
        if registry.len() >= 4096
        {
            return Err(Failure(status::LIMIT));
        }
        registry.insert(identity, Arc::downgrade(&created));
        Ok(created)
    }

    fn new(directory: Arc<Dir>, identity: [u8; 32]) -> Result<Self>
    {
        // macOS has no /proc/self/fd, and kqueue watches a descriptor
        // directly, so no path is derived at all: open a second handle to
        // the same directory for the watch. The pinned directory keeps
        // the identity stable, and the watch handle closes exactly once,
        // when this struct drops: the watcher never closes a plain fd
        // ident on drop, only filename idents.
        let watch = pin_directory_file(&directory)?;
        let mut watcher = kqueue::Watcher::new()?;
        watcher.add_file(&watch,
                         kqueue::EventFilter::EVFILT_VNODE,
                         kqueue::FilterFlag::NOTE_DELETE
                         | kqueue::FilterFlag::NOTE_WRITE
                         | kqueue::FilterFlag::NOTE_EXTEND
                         | kqueue::FilterFlag::NOTE_ATTRIB
                         | kqueue::FilterFlag::NOTE_LINK
                         | kqueue::FilterFlag::NOTE_RENAME
                         | kqueue::FilterFlag::NOTE_REVOKE)?;
        watcher.watch()?;
        Ok(Self { watcher,
                  _watch: watch,
                  generation: Mutex::new(0),
                  identity,
                  _pin: directory })
    }

    pub fn identity(&self) -> [u8; 32]
    {
        self.identity
    }

    pub fn notify(&self) -> Result<()>
    {
        let mut generation = self.generation.lock().map_err(|_| Failure(status::IO))?;
        *generation = generation.checked_add(1).ok_or(Failure(status::LIMIT))?;
        Ok(())
    }

    pub fn generation(&self) -> Result<u64>
    {
        let mut generation = self.generation.lock().map_err(|_| Failure(status::IO))?;
        // kqueue directory watches carry no file name, so unlike inotify
        // there is no way to filter private `.system-risc-` staging
        // records out of the event stream. Every event therefore
        // invalidates: that over-invalidates during publication
        // transactions (extra re-enumeration), but it can never miss a
        // real change, which is the direction the guarantee runs.
        // Error events invalidate for the same reason. The drain is
        // bounded like the Linux batch: leftover events stay queued for
        // the next call, so bounding can only defer a bump, never lose
        // one. The watcher registers EV_CLEAR, so a drained queue stays
        // drained until a new change arrives.
        let mut changed = false;
        let mut drained = 0;
        while drained < 256
        {
            match self.watcher.poll(None)
            {
                Some(_) =>
                {
                    changed = true;
                    drained += 1;
                }
                None => break
            }
        }
        if changed
        {
            *generation = generation.checked_add(1).ok_or(Failure(status::LIMIT))?;
        }
        Ok(*generation)
    }
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) struct Changes;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
impl Changes
{
    pub fn shared(_: Arc<Dir>) -> Result<Arc<Self>>
    {
        Err(Failure(status::UNSUPPORTED))
    }

    pub fn generation(&self) -> Result<u64>
    {
        Err(Failure(status::UNSUPPORTED))
    }

    pub fn identity(&self) -> [u8; 32]
    {
        [0; 32]
    }

    pub fn notify(&self) -> Result<()>
    {
        Err(Failure(status::UNSUPPORTED))
    }
}

#[cfg(windows)]
#[path = "windows_changes.rs"]
mod windows;
#[cfg(windows)]
pub(crate) use windows::Changes;
#[cfg(windows)]
pub(crate) use windows::acknowledge_directory as windows_acknowledge_directory;

#[cfg(not(windows))]
impl Changes
{
    pub fn acknowledge_child(&self, _name: &str) -> Result<()>
    {
        Ok(())
    }
}

#[cfg(windows)]
pub(crate) use windows::renamed as windows_renamed;
