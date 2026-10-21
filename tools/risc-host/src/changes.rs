//! Nonblocking host notifications include child content/metadata edits, which
//! directory mtime alone cannot see. Reads are bounded to one 4 KiB event batch.
#![forbid(unsafe_code)]
#[cfg(target_os = "linux")]
use std::{mem::MaybeUninit,
          os::fd::{AsRawFd, OwnedFd},
          sync::Mutex};

use super::*;
#[cfg(target_os = "linux")]
pub(crate) struct Changes
{
    fd:         OwnedFd,
    generation: Mutex<u64>
}
#[cfg(target_os = "linux")]
impl Changes
{
    pub fn new(directory: &Dir) -> Result<Self>
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
                           | W::MOVE_SELF
                           | W::DELETE_SELF).map_err(io::Error::from)?;
        Ok(Self { fd,
                  generation: Mutex::new(0) })
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
                Ok(_) => changed = true, // Overflow also invalidates the snapshot.
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
    pub fn new(_: &Dir) -> Result<Self>
    {
        Err(Failure(status::UNSUPPORTED))
    }

    pub fn generation(&self) -> Result<u64>
    {
        Err(Failure(status::UNSUPPORTED))
    }
}
