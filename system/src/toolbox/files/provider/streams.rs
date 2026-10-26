//! Provider streaming marks share the native writer's resident object table.
use super::{super::streams::{Progress, Remote, Stream},
            *};
impl Manager
{
    pub(in crate::toolbox::files) fn stream_provider<'a>(&self,
                                                         owner:impl Into<super::super::subject::Access<'a>>,
                                                         handle: u64)
                                                         -> Result<Option<u64>, Status>
    {
        let mark = self.streams.get_for(owner.into(), handle)?;
        Ok(mark.progress.lock().remote.as_ref().map(|remote| remote.provider))
    }

    pub(in crate::toolbox::files) fn reserve_remote_stream_authorized(
        &mut self, owner: super::super::subject::Access<'_>,
        authority: &super::super::subject::Authority, call: &ForkWriteCall, name: &str)
        -> Result<(u64, u64), Status>
    {
        self.validate_creation(owner, authority, crate::toolbox::handles::KindCode(*b"FSWR"))?;
        #[cfg(target_arch = "riscv64")]
        self.prepare_streams(crate::toolbox::handles::file_stream_kinds()?)?;
        self.validate_transfer_mutation(owner, call.volume)?;
        if call.expected_cnid != 0
           && self.pins
                  .objects()
                  .any(|pin| pin.volume == call.volume && pin.id == call.expected_cnid)
        {
            return Err(SERVICE_BUSY);
        }
        self.validate_provider_mutation(call.volume)?;
        let provider = self.read_provider(call.volume)?.ok_or(BAD_ARGUMENT)?;
        if call.parent < ROOT_ID || call.expected_cnid != 0 && call.expected_cnid <= ROOT_ID
        {
            return Err(BAD_ARGUMENT);
        }
        if self.handles
               .iter()
               .any(|(_, file)| file.volume == call.volume && file.id == call.expected_cnid)
           || self.writes.iter().any(|(_, mark)| {
                                    mark.volume == call.volume
                                    && call.expected_cnid != 0
                                    && mark.expected_cnid == call.expected_cnid
                                })
        {
            return Err(SERVICE_BUSY);
        }
        let (name, comparison) =
            mac_filesystem::Format::HfsB.prepare_name(name).map_err(|_| BAD_ARGUMENT)?;
        if self.streams.iter().any(|(_, mark)| {
                                  mark.volume == call.volume
                                  && (call.expected_cnid != 0
                                      && mark.expected == call.expected_cnid
                                      || mark.parent == call.parent
                                         && mark.comparison == comparison)
                              })
        {
            return Err(SERVICE_BUSY);
        }
        #[cfg(target_arch = "riscv64")]
        let io_subject = Some(crate::drivers::jobs::subjects::resident()?.admit(authority)?);
        let handle = self.streams
                         .insert_authorized(authority, Stream { #[cfg(target_arch = "riscv64")] io_subject, owner:owner.label(),
                                          authority: authority.clone(),
                                          volume: call.volume,
                                          parent: call.parent,
                                          expected: call.expected_cnid,
                                          when: call.when,
                                          name,
                                          comparison,
                                          progress: spinning_top::Spinlock::new(Progress {
                                            abandoned: false, quarantined: false, closed:false,
                                            state: None, published: None,
                                            remote: Some(Remote { provider,
                                                                running: true,
                                                                confirmed: None,
                                                                lengths: [call.data_length,
                                                                          call.resource_length],
                                                                written: [0; 2] }) }) })
                         ?;
        Ok((provider, handle))
    }

    pub(in crate::toolbox::files) fn remote_stream_begin(&mut self, handle: u64, bytes: &[u8],
                                                         properties: fs::FileProperties<'_>)
                                                         -> Result<(), Status>
    {
        let mark = self.streams.completing(handle).ok_or(NOT_FOUND)?;
        let mut progress = mark.progress.lock();
        let remote = progress.remote.as_mut().ok_or(BAD_ARGUMENT)?;
        let entry = catalog(mark.volume,
                            bytes,
                            (mark.expected != 0).then_some(mark.expected),
                            Some(mark.parent))?;
        if entry.is_directory()
           || entry.data_length != remote.lengths[0]
           || entry.resource_length != remote.lengths[1]
           || entry.name().as_bytes() != mark.name
           || entry.created != properties.created
           || entry.modified != properties.modified
           || entry.finder_info != properties.finder_info
           || entry.flags != properties.attributes
           || entry.backup_present != u32::from(properties.backup.is_some())
           || entry.backup != properties.backup.unwrap_or(0)
           || fs::Metadata::decode(bytes).map_err(|_| BAD_ABI)?.comment != properties.comment
        {
            return Err(BAD_ABI);
        }
        remote.confirmed = Some(entry);
        remote.running = false;
        Ok(())
    }

    pub(in crate::toolbox::files) fn remote_stream_action<'a>(
        &mut self, owner: impl Into<super::super::subject::Access<'a>>, call: &ForkWriteCall)
        -> Result<(u64, fs::Arguments<'static>), Status>
    {
        let owner = owner.into();
        let mark = self.streams.get_for(owner, call.handle)?;
        let mut progress = mark.progress.lock();
        if progress.quarantined
        {
            return Err(ROLLBACK_FAILED);
        }
        if progress.abandoned
        {
            return Err(OPERATION_CANCELLED);
        }
        let remote = progress.remote.as_mut().ok_or(BAD_ARGUMENT)?;
        if remote.running
        {
            return Err(SERVICE_BUSY);
        }
        let index = call.fork as usize;
        if index > 1
        {
            return Err(BAD_ARGUMENT);
        }
        match call.operation
        {
            APPEND_FORK_WRITE =>
            {
                let end =
                    remote.written[index].checked_add(call.count as u64).ok_or(BAD_ARGUMENT)?;
                if call.count == 0
                   || end > remote.lengths[index]
                   || end != remote.lengths[index]
                      && !call.count.is_multiple_of(fs::WRITE_ALIGNMENT)
                {
                    return Err(BAD_ARGUMENT);
                }
            }
            READ_FORK_WRITE =>
            {
                if call.offset > remote.written[index]
                {
                    return Err(BAD_ARGUMENT);
                }
            }
            FINISH_FORK_WRITE =>
            {
                if remote.written != remote.lengths
                {
                    return Err(BAD_ARGUMENT);
                }
            }
            ABORT_FORK_WRITE => (),
            _ => return Err(BAD_ARGUMENT)
        }
        remote.running = true;
        Ok((remote.provider,
            fs::Arguments { volume: mark.volume,
                            token: call.handle,
                            flags: call.fork,
                            offset: call.offset,
                            length: call.capacity as u64,
                            ..Default::default() }))
    }

    pub(in crate::toolbox::files) fn remote_stream_ready(&mut self, handle: u64, fork: u32,
                                                         count: usize)
                                                         -> Result<(), Status>
    {
        let mark = self.streams.completing(handle).ok_or(NOT_FOUND)?;
        let mut progress = mark.progress.lock();
        let remote = progress.remote.as_mut().ok_or(BAD_ARGUMENT)?;
        let written = remote.written.get_mut(fork as usize).ok_or(BAD_ARGUMENT)?;
        *written = written.checked_add(count as u64).ok_or(BAD_ABI)?;
        remote.running = false;
        Ok(())
    }

    pub(in crate::toolbox::files) fn publish_remote_stream(&mut self, handle: u64,
                                                           receipt: fs::WriteReceipt<'_>)
                                                           -> Result<CatalogInfo, Status>
    {
        let mark = self.streams.completing(handle).ok_or(NOT_FOUND)?;
        let progress = mark.progress.lock();
        let remote = progress.remote.as_ref().ok_or(BAD_ARGUMENT)?;
        if receipt.file.id != remote.confirmed.ok_or(BAD_ABI)?.id
        {
            return Err(BAD_ABI);
        }
        let mut bytes = [0; fs::MAX_METADATA_BYTES];
        let n = receipt.file.encode(&mut bytes).map_err(|_| BAD_ABI)?;
        let entry = catalog(mark.volume, &bytes[..n], Some(receipt.file.id), Some(mark.parent))?;
        if remote.confirmed != Some(entry)
        {
            return Err(BAD_ABI);
        }
        let (volume, parent) = (mark.volume, mark.parent);
        let info = volume_info(volume, receipt.volume);
        drop(progress);
        let revision = super::super::scan::next_revision()?;
        let mounted = self.provider_mount(volume)?;
        let prior = mounted.info.ok_or(BAD_ABI)?;
        if info.filesystem != prior.filesystem
           || info.filesystem_version != prior.filesystem_version
        {
            return Err(BAD_ABI);
        }
        mounted.info = Some(info);
        mounted.revision = revision;
        // Keep the mark until notification succeeds; a failed acknowledgement
        // must not turn a committed file into a vanished owner.
        let mark = self.streams.completing(handle).ok_or(NOT_FOUND)?;
        let mut progress = mark.progress.lock();
        progress.published = Some(entry);
        progress.closed = true;
        drop(progress);
        self.notify_directory_at(volume, parent, revision);
        self.streams.remove(handle).ok_or(NOT_FOUND)?;
        Ok(entry)
    }

    pub(in crate::toolbox::files) fn retire_remote_stream(&mut self, handle: u64)
                                                          -> Result<(), Status>
    {
        let mark = self.streams.completing(handle).ok_or(NOT_FOUND)?;
        let mut progress = mark.progress.lock();
        if progress.remote.is_none()
        {
            return Err(BAD_ARGUMENT);
        }
        progress.closed = true;
        drop(progress);
        self.streams.remove(handle).ok_or(NOT_FOUND)?;
        Ok(())
    }

    pub(in crate::toolbox::files) fn quarantine_remote_stream(&mut self, handle: u64)
    {
        if let Some(mark) = self.streams.completing(handle)
        {
            mark.progress.lock().quarantined = true;
        }
    }
}
