//! Owned, nonblocking change notifications. Polling never performs filesystem I/O.
use mac_abi::files::{DISPOSE_WATCH, FileCall, POLL_WATCH, WATCH_DIRECTORY};

use super::{Cnid, Files, Result, VolumeId};

pub struct DirectoryWatch<'a>
{
    files: Files<'a>,
    id:    u32
}
impl<'a> Files<'a>
{
    /// Changes coalesce until polled. Read-only volumes accept watches too;
    /// immutable backends need no physical monitoring.
    pub fn watch_directory(self, volume: VolumeId, directory: Cnid) -> Result<DirectoryWatch<'a>>
    {
        let call = self.call(FileCall { operation: WATCH_DIRECTORY,
                                        volume: volume.0,
                                        cnid: directory.0,
                                        ..Default::default() },
                             "")?;
        Ok(DirectoryWatch { files: self,
                            id:    call.handle })
    }
}
impl DirectoryWatch<'_>
{
    pub fn id(&self) -> u32
    {
        self.id
    }

    /// Return the new catalog revision once per coalesced change, or None.
    /// Remote probes run on the resident background queue, outside this call.
    pub fn poll(&self) -> Result<Option<u64>>
    {
        let call = self.files.call(FileCall { operation: POLL_WATCH,
                                               handle: self.id,
                                               ..Default::default() },
                                    "")?;
        Ok((call.index != 0).then_some(call.position))
    }

    pub fn close(mut self) -> Result<()>
    {
        self.files.call(FileCall { operation: DISPOSE_WATCH,
                                    handle: self.id,
                                    ..Default::default() },
                         "")?;
        self.id = 0;
        Ok(())
    }
}
impl Drop for DirectoryWatch<'_>
{
    fn drop(&mut self)
    {
        if self.id != 0
        {
            let _ = self.files.call(FileCall { operation: DISPOSE_WATCH,
                                               handle: self.id,
                                               ..Default::default() },
                                    "");
        }
    }
}
