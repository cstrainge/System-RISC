//! Trusted positioned backing reads. Marks belong to an admitted operation
//! scope, retaining their catalog/volume through actual backend acknowledgement.
#![forbid(unsafe_code)]
use super::*;
impl Manager
{
    pub(crate) fn source_provider(&self, volume: u32) -> Result<Option<u64>, Status>
    {
        self.read_provider(volume)
    }

    pub(crate) fn reserve_provider_source(&mut self, scope: &subject::Lease, volume: u32,
                                          cnid: u32)
                                          -> Result<(u64, u32), Status>
    {
        self.reserve_provider_file_authorized(scope.into(),
                                              &subject::Authority::Operation(scope.clone()),
                                              volume,
                                              cnid,
                                              DATA_FORK)
    }

    pub(crate) fn source_mark_provider(&self, scope: &subject::Lease, handle: u32)
                                       -> Result<Option<u64>, Status>
    {
        self.file_provider(scope, handle)
    }

    pub(crate) fn publish_provider_source(&mut self, scope: &subject::Lease, handle: u32,
                                          bytes: &[u8])
                                          -> Result<u64, Status>
    {
        self.slot_access(scope.into(), handle)?;
        self.publish_provider_file(handle, bytes)
    }

    pub(crate) fn begin_provider_source_read(
        &mut self, scope: &subject::Lease, handle: u32, offset: u64, length: usize)
        -> Result<(u64, mac_filesystem_interface::Arguments<'static>), Status>
    {
        let file = self.slot_access(scope.into(), handle)?;
        let fork_guard = file.fork.lock();
        let FileFork::Provider(fork) = &*fork_guard
        else
        {
            return Err(BAD_ARGUMENT);
        };
        if offset > fork.ready_length()?
        {
            return Err(BAD_ARGUMENT);
        }
        // This mark is private to the source lease, never a public app mark.
        file.position.store(offset, core::sync::atomic::Ordering::Relaxed);
        drop(fork_guard);
        self.begin_provider_file_read(scope, handle, length)
    }

    pub(crate) fn finish_provider_source_read(&mut self, scope: &subject::Lease, handle: u32,
                                              offset: u64, length: u64, delivered: Option<usize>)
                                              -> Result<(), Status>
    {
        self.slot_access(scope.into(), handle)?;
        self.finish_provider_file_read(handle, offset, length, delivered).map(|_| ())
    }

    pub(crate) fn close_provider_source<R>(&mut self, scope: &subject::Lease, handle: u32,
                                           cleanup: bool,
                                           publish: impl FnOnce(u64,
                                                  mac_filesystem_interface::Arguments<'static>)
                                                  -> Result<R, Status>)
                                           -> Result<R, Status>
    {
        self.publish_provider_file_close(scope.into(), handle, cleanup, publish)
    }

    pub(crate) fn retire_provider_source(&mut self, scope: &subject::Lease, handle: u32)
                                         -> Result<(), Status>
    {
        self.slot_access(scope.into(), handle)?;
        self.retire_provider_file(handle)
    }

    pub(crate) fn quarantine_provider_source(&mut self, scope: &subject::Lease, handle: u32)
                                             -> Result<(), Status>
    {
        self.slot_access(scope.into(), handle)?;
        self.quarantine_provider_file(handle)
    }

    pub(crate) fn describe_block_source(&mut self, volume: u32, cnid: u32)
                                        -> Result<Vec<u8>, Status>
    {
        let entry = self.entry(volume, cnid)?;
        if entry.directory
        {
            return Err(BAD_ARGUMENT);
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(mac_filesystem_interface::MAX_METADATA_BYTES)
             .map_err(|_| NO_MEMORY)?;
        bytes.resize(mac_filesystem_interface::MAX_METADATA_BYTES, 0);
        let name = core::str::from_utf8(&entry.name).map_err(|_| BAD_ABI)?;
        let count = mac_filesystem_interface::Metadata { id: entry.id,
                                                         parent: entry.parent,
                                                         directory: false,
                                                         data_length:
                                                             entry.data
                                                                  .as_ref()
                                                                  .map_or(0, |fork| fork.length),
                                                         resource_length:
                                                             entry.resources
                                                                  .as_ref()
                                                                  .map_or(0, |fork| fork.length),
                                                         created: entry.created,
                                                         modified: entry.modified,
                                                         backup: entry.backup,
                                                         child_count: 0,
                                                         attributes: 0,
                                                         finder_info: entry.finder_info,
                                                         revision: 1,
                                                         name,
                                                         comment: "" }.encode(&mut bytes)
                                                                      .map_err(|_| BAD_ABI)?;
        bytes.truncate(count);
        Ok(bytes)
    }

    pub(crate) fn open_block_source(&mut self, scope: &subject::Lease, volume: u32, cnid: u32)
                                    -> Result<(u32, u64), Status>
    {
        if self.read_provider(volume)?.is_some()
        {
            return Err(NOT_SUPPORTED);
        }
        self.open_authorized(scope.into(),
                             &subject::Authority::Operation(scope.clone()),
                             volume,
                             cnid,
                             DATA_FORK)
    }

    pub(crate) fn read_block_source(&mut self, scope: &subject::Lease, handle: u32, offset: u64,
                                    output: &mut [u8])
                                    -> Result<usize, Status>
    {
        if output.len() > mac_file_source_interface::MAX_TRANSFER
        {
            return Err(BAD_ARGUMENT);
        }
        let (volume, mut fork) = {
            let file = self.slot_access(scope.into(), handle)?;
            let FileFork::Block(fork) = &*file.fork.lock()
            else
            {
                return Err(NOT_SUPPORTED);
            };
            if offset > fork.length
            {
                return Err(BAD_ARGUMENT);
            }
            (file.volume, fork.clone())
        };
        let read = self.with_volume(volume, |volume| io(volume.read(&mut fork, offset, output)))?;
        *self.slot_access(scope.into(), handle)?.fork.lock() = FileFork::Block(fork);
        Ok(read)
    }

    pub(crate) fn close_block_source(&mut self, scope: &subject::Lease, handle: u32)
                                     -> Result<(), Status>
    {
        let file = self.slot_access(scope.into(), handle)?;
        if !matches!(&*file.fork.lock(), FileFork::Block(_))
        {
            return Err(NOT_SUPPORTED);
        }
        self.handles.remove(handle).ok_or(NOT_FOUND)?;
        Ok(())
    }
}
