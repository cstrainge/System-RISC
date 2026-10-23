//! Resident owners for bounded fork writes. Failed publication/cleanup retains
//! its generation-tagged mark, keeping the volume and its provisional data owned.
use core::sync::atomic::{AtomicBool, Ordering};

use super::*;
fn stream_status(error: mac_filesystem::stream::StreamError) -> Status
{
    use mac_filesystem::stream::{NativeError as N, StreamError as E};
    match error
    {
        E::Unsupported => NOT_SUPPORTED,
        E::Policy(error) => write::status(error),
        E::Native(N::Invalid) => BAD_ARGUMENT,
        E::Native(N::Incompatible) => BAD_ABI,
        E::Native(N::Corrupt) => CORRUPT_DATA,
        E::Native(N::Stale) => SCAN_STALE,
        E::Native(N::NotFound) => NOT_FOUND,
        E::Native(N::Exists) => ALREADY_EXISTS,
        E::Native(N::Protected) => ACCESS_DENIED,
        E::Native(N::ReadOnly) => READ_ONLY,
        E::Native(N::Full) => DISK_FULL,
        E::Native(N::Budget | N::NoMemory) => NO_MEMORY,
        E::Native(N::RollbackFailed | N::Dirty) => ROLLBACK_FAILED,
        E::Native(N::Io) => IO_ERROR
    }
}
static CLEANUP: AtomicBool = AtomicBool::new(false);
pub(super) fn request_cleanup()
{
    CLEANUP.store(true, Ordering::Release);
}
pub(super) fn needs_cleanup() -> bool
{
    CLEANUP.load(Ordering::Acquire)
}
pub(super) struct Stream
{
    pub owner:       u64,
    pub volume:      u32,
    pub parent:      u32,
    pub expected:    u32,
    pub abandoned:   bool,
    pub quarantined: bool,
    pub when:        u64,
    pub name:        Vec<u8>,
    pub comparison:  Vec<u8>,
    pub state:       Option<mac_filesystem::stream::StagedFile>,
    pub published:   Option<CatalogInfo>,
    pub remote:      Option<Remote>
}
pub(super) struct Remote
{
    pub provider:  u32,
    pub running:   bool,
    pub confirmed: Option<CatalogInfo>,
    pub lengths:   [u64; 2],
    pub written:   [u64; 2]
}
impl Manager
{
    fn stream_protection(&self, volume: u32, except: u32) -> Result<Vec<u32>, Status>
    {
        let mut protected = Vec::new();
        protected.try_reserve_exact(self.handles.len() + self.writes.len() + self.streams.len())
                 .map_err(|_| NO_MEMORY)?;
        protected.extend(self.handles
                             .iter()
                             .filter(|(_, file)| file.volume == volume)
                             .map(|(_, file)| file.id));
        protected.extend(self.writes
                             .iter()
                             .filter(|(_, file)| file.volume == volume)
                             .map(|(_, file)| file.expected_cnid)
                             .filter(|id| *id != 0));
        protected.extend(self.streams
                             .iter()
                             .filter(|(id, mark)| *id != except && mark.volume == volume)
                             .map(|(_, mark)| mark.expected)
                             .filter(|id| *id != 0));
        Ok(protected)
    }

    pub(super) fn stream_request(&mut self, owner: u64, call: &mut ForkWriteCall, name: &str,
                                 comment: &str, data: &[u8], output: &mut [u8])
                                 -> Result<(), Status>
    {
        if call.padding != 0
           || call.reserved != 0
           || call.backup_present > 1
           || call.backup_present == 0 && call.backup != 0
           || call.fork > RESOURCE_FORK
        {
            return Err(BAD_ARGUMENT);
        }
        call.completed = 0;
        call.transferred = 0;
        if call.operation == BEGIN_FORK_WRITE
        {
            self.validate_transfer_mutation(owner, call.volume)?;
            if call.expected_cnid != 0 && self.file_pinned(call.volume, call.expected_cnid)
            {
                return Err(SERVICE_BUSY);
            }
            self.validate_provider_mutation(call.volume)?;
            if self.read_provider(call.volume)?.is_some()
            {
                return Err(NOT_SUPPORTED);
            }
            let (name, comparison) =
                mac_filesystem::Format::HfsB.prepare_name(name).map_err(|_| BAD_ARGUMENT)?;
            if self.streams.iter().any(|(_, stream)| {
                                      stream.volume == call.volume
                                      && stream.parent == call.parent
                                      && stream.comparison == comparison
                                  })
            {
                return Err(SERVICE_BUSY);
            }
            let protected = self.stream_protection(call.volume, 0)?;
            let mounted = self.mounted(call.volume)?;
            if mounted.info.read_only != 0
            {
                return Err(READ_ONLY);
            }
            if mounted.poisoned
            {
                return Err(ROLLBACK_FAILED);
            }
            if mounted.info.filesystem != FILESYSTEM_HFS_B
            {
                return Err(NOT_SUPPORTED);
            }
            self.streams.reserve().map_err(|_| NO_MEMORY)?;
            let handle = self.streams
                             .insert(Stream { owner,
                                              volume: call.volume,
                                              parent: call.parent,
                                              expected: call.expected_cnid,
                                              abandoned: false,
                                              quarantined: false,
                                              when: call.when,
                                              name,
                                              comparison,
                                              state: None,
                                              published: None,
                                              remote: None })
                             .map_err(|_| NO_MEMORY)?;
            let begun = (|| {
                let (volumes, streams) = (&mut self.volumes, &mut self.streams);
                let mark = streams.get_mut(handle).ok_or(NOT_FOUND)?;
                let Mounted::Block(mounted) = volumes.get_mut(mark.volume).ok_or(NOT_FOUND)?
                else
                {
                    return Err(NOT_SUPPORTED);
                };
                let mut volume = io(Volume::mount(&mut mounted.device, mounted.partition))?;
                let spec=mac_filesystem::stream::StreamSpec{parent:mark.parent,
                    name:core::str::from_utf8(&mark.name).map_err(|_|BAD_ARGUMENT)?,
                    expected:mark.expected,data_length:call.data_length,resource_length:call.resource_length,
                    flags:call.attributes,finder_info:call.finder_info,created:call.created,
                    modified:call.modified,backup:(call.backup_present!=0).then_some(call.backup),comment};
                let state =
                    volume.begin_stream(spec, call.when, &protected).map_err(stream_status)?;
                mark.state = Some(state);
                mounted.scanner = volume.catalog_scanner().ok();
                mounted.info.free_bytes = volume.free_bytes;
                Ok(())
            })();
            if let Err(error) = begun
            {
                // begin_stream cannot fail after it publishes the provisional
                // file. Its only earlier side effect is a valid empty staging dir.
                self.streams.remove(handle);
                return Err(error);
            }
            call.handle = handle;
            return Ok(());
        }
        let mark = self.streams.get_mut(call.handle).ok_or(NOT_FOUND)?;
        if mark.owner != owner
        {
            return Err(ACCESS_DENIED);
        }
        if call.operation == ABANDON_FORK_WRITE
        {
            mark.abandoned = true;
            self.stream_cleanup = true;
            request_cleanup();
            return Ok(());
        }
        if mark.quarantined
        {
            return Err(ROLLBACK_FAILED);
        }
        if mark.abandoned
        {
            return Err(OPERATION_CANCELLED);
        }
        self.step_stream(call, data, output)
    }

    fn step_stream(&mut self, call: &mut ForkWriteCall, data: &[u8], output: &mut [u8])
                   -> Result<(), Status>
    {
        let volume = self.streams.get(call.handle).ok_or(NOT_FOUND)?.volume;
        let protected = if call.operation == FINISH_FORK_WRITE
                           && self.streams.get(call.handle).unwrap().published.is_none()
        {
            self.stream_protection(volume, call.handle)?
        }
        else
        {
            Vec::new()
        };
        let (volumes, streams) = (&mut self.volumes, &mut self.streams);
        let mark = streams.get_mut(call.handle).ok_or(NOT_FOUND)?;
        let Mounted::Block(mounted) = volumes.get_mut(mark.volume).ok_or(NOT_FOUND)?
        else
        {
            return Err(NOT_SUPPORTED);
        };
        if mounted.poisoned
        {
            return Err(ROLLBACK_FAILED);
        }
        let mut volume = io(Volume::mount(&mut mounted.device, mounted.partition))?;
        let state = mark.state.as_mut().ok_or(SERVICE_BUSY)?;
        let mut terminal = false;
        let mut notify = None;
        let result = (|| {
            match call.operation
            {
                APPEND_FORK_WRITE =>
                {
                    volume.append_stream(state, call.fork == RESOURCE_FORK, data, mark.when)
                          .map_err(stream_status)?
                }
                READ_FORK_WRITE =>
                {
                    call.transferred =
                        volume.read_stream(state, call.fork == RESOURCE_FORK, call.offset, output)
                              .map_err(stream_status)?
                }
                FINISH_FORK_WRITE =>
                {
                    if mark.published.is_none()
                    {
                        let entry = volume.finish_stream(state, mark.when, &protected)
                                          .map_err(stream_status)?;
                        mark.published = Some(metadata(mark.volume, &entry));
                        notify = Some((mark.volume, mark.parent));
                    }
                    let cleanup = volume.abort_stream(state, mark.when).map_err(stream_status)?;
                    terminal = cleanup != mac_filesystem::stream::StreamAbort::Pending;
                    call.entry = mark.published.ok_or(BAD_ABI)?;
                }
                ABORT_FORK_WRITE =>
                {
                    let cleanup = volume.abort_stream(state, mark.when).map_err(stream_status)?;
                    terminal = cleanup != mac_filesystem::stream::StreamAbort::Pending;
                    if let Some(entry) = mark.published
                    {
                        call.entry = entry;
                    }
                }
                _ => return Err(BAD_ARGUMENT)
            }
            Ok(())
        })();
        mounted.scanner = volume.catalog_scanner().ok();
        mounted.info.free_bytes = volume.free_bytes;
        mounted.info.modified = volume.modified;
        if result == Err(ROLLBACK_FAILED)
        {
            mark.quarantined = true;
            mounted.poisoned = true;
            mounted.info.read_only = 1;
        }
        // Notification is once per publication, even if later old-payload
        // reclamation fails. The committed destination already exists.
        if let Some((volume, parent)) = notify
        {
            self.notify_directory(volume, parent)?;
        }
        result?;
        if terminal
        {
            self.streams.remove(call.handle).ok_or(NOT_FOUND)?;
            call.completed = 1;
        }
        Ok(())
    }

    /// At most one chunk of abandoned storage per scheduler pass. Never enter
    /// this loop on every input event when there is no abandoned owner.
    pub(super) fn pump_stream_cleanup(&mut self)
    {
        if !self.stream_cleanup
        {
            return;
        }
        let Some(handle) = self.streams.iter().find_map(|(id, mark)| {
                                                  (mark.abandoned
                                                   && !mark.quarantined
                                                   && mark.remote.is_none()).then_some(id)
                                              })
        else
        {
            self.stream_cleanup = false;
            CLEANUP.store(false, Ordering::Release);
            return;
        };
        let mut call = ForkWriteCall { operation: ABORT_FORK_WRITE,
                                       handle,
                                       ..Default::default() };
        if self.step_stream(&mut call, &[], &mut []).is_err()
        {
            if let Some(mark) = self.streams.get_mut(handle)
            {
                mark.quarantined = true;
            }
        }
    }
}
