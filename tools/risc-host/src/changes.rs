//! Nonblocking host notifications include child content/metadata edits, which
//! directory mtime alone cannot see. Reads are bounded to one 4 KiB event batch.
#![forbid(unsafe_code)]
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
        let metadata = directory.open(".")?.into_std().metadata()?;
        let identity = if let Some(identity) = object_identity(&metadata)?
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
#[cfg(not(target_os = "linux"))]
pub(crate) struct Changes;
#[cfg(not(target_os = "linux"))]
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
