//! Resident owners for bounded fork writes. Failed publication/cleanup retains
//! its generation-tagged mark, keeping the volume and its provisional data owned.
use core::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::toolbox::handles::OwnerRef;
pub(crate) mod lifetime;
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
pub(super) static SCOPES: super::objects::ScopeChanges = super::objects::ScopeChanges::new();
pub(super) fn needs_cleanup() -> bool
{
    SCOPES.pending() || CLEANUP.load(Ordering::Acquire)
}
pub(super) struct Stream
{
    pub owner:      u64,
    pub volume:     u32,
    pub parent:     u32,
    pub expected:   u32,
    pub authority:  super::subject::Authority,
    pub when:       u64,
    pub name:       Vec<u8>,
    pub comparison: Vec<u8>,
    pub progress:   spinning_top::Spinlock<Progress>
}
pub(super) struct Progress
{
    pub abandoned:   bool,
    pub quarantined: bool,
    pub closed:      bool,
    pub state:       Option<mac_filesystem::stream::StagedFile>,
    pub published:   Option<CatalogInfo>,
    pub remote:      Option<Remote>
}
impl super::objects::Payload for Stream
{
    type Storage = crate::toolbox::files::objects::storage::Native<Self>;

    const KIND: crate::toolbox::handles::KindCode = crate::toolbox::handles::KindCode(*b"FSWR");

    fn cleanup_pending(&self) -> bool
    {
        let progress = self.progress.lock();
        !progress.closed && (progress.state.is_some() || progress.remote.is_some())
    }

    fn begin_cleanup(&self)
    {
        let mut progress = self.progress.lock();
        progress.abandoned = true;
        request_cleanup();
    }

    fn scope(&self) -> Option<&super::subject::Lease>
    {
        self.authority.scope()
    }

    fn access_allowed(&self, access: super::subject::Access<'_>) -> bool
    {
        self.authority.matches(self.owner, access)
    }

    fn owner_stopping(&self) -> bool
    {
        !self.authority.accepting()
    }
}
impl lifetime::StreamHost for Manager
{
    fn release_file_stream(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                           storage: bool)
                           -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        self.streams.release(ticket, storage)
    }
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
    fn stream_protection(&self, volume: u32, except: u64) -> Result<Vec<u32>, Status>
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
        if call.operation == BEGIN_FORK_WRITE
        {
            #[cfg(test)]
            let authority = self.fixture_stream_owner(owner);
            #[cfg(all(not(test), target_arch = "riscv64"))]
            let authority = if owner == 0
            {
                crate::toolbox::handles::system_owner()?
            }
            else
            {
                return Err(ACCESS_DENIED);
            };
            #[cfg(all(not(test), not(target_arch = "riscv64")))]
            return Err(ACCESS_DENIED);
            #[cfg(any(test, target_arch = "riscv64"))]
            return self.stream_request_for(owner, &authority, call, name, comment, data, output);
        }
        self.stream_request_inner(super::subject::Access::Client(owner),
                                  None,
                                  call,
                                  name,
                                  comment,
                                  data,
                                  output)
    }

    pub(super) fn stream_request_for(&mut self, owner: u64, authority: &OwnerRef,
                                     call: &mut ForkWriteCall, name: &str, comment: &str,
                                     data: &[u8], output: &mut [u8])
                                     -> Result<(), Status>
    {
        self.stream_request_authorized(super::subject::Access::Client(owner),
                                       Some(&super::subject::Authority::Client(authority.clone())),
                                       call,
                                       name,
                                       comment,
                                       data,
                                       output)
    }

    pub(super) fn stream_request_authorized(&mut self, owner: super::subject::Access<'_>,
                                            authority: Option<&super::subject::Authority>,
                                            call: &mut ForkWriteCall, name: &str, comment: &str,
                                            data: &[u8], output: &mut [u8])
                                            -> Result<(), Status>
    {
        if let Some(authority) = authority
        {
            self.validate_creation(owner, authority, crate::toolbox::handles::KindCode(*b"FSWR"))?;
        }
        #[cfg(target_arch = "riscv64")]
        self.prepare_streams(crate::toolbox::handles::file_stream_kinds()?)?;
        self.stream_request_inner(owner, authority, call, name, comment, data, output)
    }

    fn stream_request_inner(&mut self, owner: super::subject::Access<'_>,
                            authority: Option<&super::subject::Authority>,
                            call: &mut ForkWriteCall, name: &str, comment: &str, data: &[u8],
                            output: &mut [u8])
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
            let authority = authority.ok_or(ACCESS_DENIED)?;
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
                             .insert_authorized(authority, Stream { owner:owner.label(),
                                              authority: authority.clone(),
                                              volume: call.volume,
                                              parent: call.parent,
                                              expected: call.expected_cnid,
                                              when: call.when,
                                              name,
                                              comparison,
                                              progress: spinning_top::Spinlock::new(Progress {
                                                  abandoned: false, quarantined: false, closed: false,
                                                  state: None, published: None, remote: None }) })
                             ?;
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
                mark.progress.lock().state = Some(state);
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
        let mark = self.streams.get_for(owner, call.handle)?;
        if call.operation == ABANDON_FORK_WRITE
        {
            return self.streams.abandon(call.handle);
        }
        let progress = mark.progress.lock();
        if progress.quarantined
        {
            return Err(ROLLBACK_FAILED);
        }
        if progress.abandoned
        {
            return Err(OPERATION_CANCELLED);
        }
        drop(progress);
        self.step_stream(call, data, output)
    }

    fn step_stream(&mut self, call: &mut ForkWriteCall, data: &[u8], output: &mut [u8])
                   -> Result<(), Status>
    {
        let mark = self.streams.completing(call.handle).ok_or(NOT_FOUND)?;
        let volume = mark.volume;
        let publishing =
            call.operation == FINISH_FORK_WRITE && mark.progress.lock().published.is_none();
        let revision = if publishing { Some(scan::next_revision()?) } else { None };
        let protected =
            if publishing { self.stream_protection(volume, call.handle)? } else { Vec::new() };
        let (volumes, streams) = (&mut self.volumes, &mut self.streams);
        let mark = streams.completing(call.handle).ok_or(NOT_FOUND)?;
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
        let mut progress = mark.progress.lock();
        let Progress { state,
                       published,
                       .. } = &mut *progress;
        let state = state.as_mut().ok_or(SERVICE_BUSY)?;
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
                    if published.is_none()
                    {
                        let entry = volume.finish_stream(state, mark.when, &protected)
                                          .map_err(stream_status)?;
                        *published = Some(metadata(mark.volume, &entry));
                        notify = Some((mark.volume, mark.parent));
                    }
                    let cleanup = volume.abort_stream(state, mark.when).map_err(stream_status)?;
                    terminal = cleanup != mac_filesystem::stream::StreamAbort::Pending;
                    call.entry = published.ok_or(BAD_ABI)?;
                }
                ABORT_FORK_WRITE =>
                {
                    let cleanup = volume.abort_stream(state, mark.when).map_err(stream_status)?;
                    terminal = cleanup != mac_filesystem::stream::StreamAbort::Pending;
                    if let Some(entry) = *published
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
            progress.quarantined = true;
            mounted.poisoned = true;
            mounted.info.read_only = 1;
        }
        if terminal
        {
            progress.closed = true;
        }
        drop(progress);
        // Notification is once per publication, even if later old-payload
        // reclamation fails. The committed destination already exists.
        if let Some((volume, parent)) = notify
        {
            self.notify_directory_at(volume, parent, revision.ok_or(BAD_ABI)?);
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
    pub(crate) fn pump_stream_cleanup(&mut self)
    {
        let Some(handle) = self.streams.iter().find_map(|(id, mark)| {
                                                  let progress = mark.progress.lock();
                                                  (progress.abandoned
                                                   && !progress.closed
                                                   && !progress.quarantined
                                                   && progress.remote.is_none()).then_some(id)
                                              })
        else
        {
            if !self.streams.iter().any(|(_, mark)| {
                                       let progress = mark.progress.lock();
                                       progress.abandoned
                                       && !progress.closed
                                       && !progress.quarantined
                                   })
            {
                CLEANUP.store(false, Ordering::Release);
            }
            return;
        };
        let mut call = ForkWriteCall { operation: ABORT_FORK_WRITE,
                                       handle,
                                       ..Default::default() };
        if self.step_stream(&mut call, &[], &mut []).is_err()
        {
            if let Some(mark) = self.streams.completing(handle)
            {
                mark.progress.lock().quarantined = true;
            }
        }
    }
}

#[cfg(test)]
impl Manager
{
    pub(super) fn fixture_stream_owner(&mut self, label: u64) -> crate::toolbox::handles::OwnerRef
    {
        use mac_object_table::domain::{DomainKind, OwnerIndex};

        use crate::toolbox::handles::{KindCode, OwnerRef};
        let owner = self.handles.fixture_owner(label);
        let registry = &mut self.handles.fixture.as_mut().unwrap().0;
        let kinds = match (registry.get(KindCode(*b"FSWR")), registry.get(KindCode(*b"FSST")))
        {
            (Ok(payload), Ok(storage)) => (payload, storage),
            _ =>
            {
                let registrar =
                    OwnerRef::try_new(OwnerIndex::issue(DomainKind::Module).unwrap()).unwrap();
                (registry.register(&registrar,
                                   lifetime::specification(false),
                                   lifetime::ReleaseStreams)
                         .unwrap(),
                 registry.register(&registrar,
                                   lifetime::specification(true),
                                   lifetime::ReleaseStorage)
                         .unwrap())
            }
        };
        self.prepare_streams(kinds).unwrap();
        owner
    }
}
