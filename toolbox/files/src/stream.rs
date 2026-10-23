//! Owned two-fork writes. Each step bounds work; waiting/cancellation can remain
//! part of the application's ordinary executor or event loop.
use super::*;

#[derive(Clone, Copy)]
pub struct ForkMetadata<'a>
{
    pub attributes:  CatalogFlags,
    pub finder_info: [u8; 32],
    pub created:     Timestamp,
    pub modified:    Timestamp,
    pub backup:      Option<Timestamp>,
    pub comment:     &'a str
}
impl<'a> ForkMetadata<'a>
{
    pub fn from_catalog(entry: &CatalogInfo, comment: &'a str) -> Self
    {
        let mut finder_info = entry.finder_info;
        if !entry.is_directory()
        {
            finder_info[..4].copy_from_slice(&entry.file_type.bytes());
            finder_info[4..8].copy_from_slice(&entry.creator.bytes());
        }
        Self { attributes: entry.flags,
               finder_info,
               created: entry.created,
               modified: entry.modified,
               backup: entry.backup,
               comment }
    }
}
pub struct ForkWriteSpec<'a>
{
    pub volume:          VolumeId,
    pub parent:          Cnid,
    pub name:            &'a str,
    pub replace:         Option<Cnid>,
    pub data_length:     u64,
    pub resource_length: u64,
    pub metadata:        ForkMetadata<'a>
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkAbort
{
    Pending,
    Aborted,
    /// Publication won a race with cancellation. Cleanup disposed only the old
    /// provisional payload; it never deleted the committed destination.
    Published(CatalogInfo)
}
#[must_use = "dropping an unfinished writer requests owned cleanup"]
pub struct ForkWriter<'a>
{
    client:  Client<'a>,
    handle:  u32,
    lengths: [u64; 2],
    written: [u64; 2]
}
fn fork_code(fork: Fork) -> u32
{
    match fork
    {
        Fork::Data => DATA_FORK,
        Fork::Resource => RESOURCE_FORK
    }
}
impl<'a> Files<'a>
{
    pub fn begin_fork_write(self, spec: ForkWriteSpec<'_>) -> Result<ForkWriter<'a>>
    {
        let mut call =
            ForkWriteCall { operation: BEGIN_FORK_WRITE,
                            volume: spec.volume.0,
                            parent: spec.parent.0,
                            expected_cnid: spec.replace.map_or(0, |id| id.0),
                            when: self.client
                                      .now()?
                                      .mac_microseconds()
                                      .ok_or(Error::BadArgument)?,
                            data_length: spec.data_length,
                            resource_length: spec.resource_length,
                            created: spec.metadata
                                         .created
                                         .mac_microseconds()
                                         .ok_or(Error::BadArgument)?,
                            modified: spec.metadata
                                          .modified
                                          .mac_microseconds()
                                          .ok_or(Error::BadArgument)?,
                            backup: spec.metadata
                                        .backup
                                        .map(|t| t.mac_microseconds().ok_or(Error::BadArgument))
                                        .transpose()?
                                        .unwrap_or(0),
                            backup_present: u32::from(spec.metadata.backup.is_some()),
                            attributes: spec.metadata.attributes.bits(),
                            finder_info: spec.metadata.finder_info,
                            ..Default::default() };
        self.client.fork_write(&mut call, spec.name, spec.metadata.comment, &[], &mut [])?;
        Ok(ForkWriter { client:  self.client,
                        handle:  call.handle,
                        lengths: [spec.data_length, spec.resource_length],
                        written: [0; 2] })
    }
}
impl ForkWriter<'_>
{
    /// Append to either fork independently. Requests are at most 64 KiB; every
    /// nonfinal chunk is a multiple of 4096 bytes. A final chunk may be shorter.
    pub fn write_chunk(&mut self, fork: Fork, bytes: &[u8]) -> Result<()>
    {
        if bytes.is_empty()
        {
            return Ok(());
        }
        let index = fork_code(fork) as usize;
        let end = self.written[index].checked_add(bytes.len() as u64).ok_or(Error::BadArgument)?;
        if end > self.lengths[index]
           || end != self.lengths[index] && !bytes.len().is_multiple_of(4096)
        {
            return Err(Error::BadArgument);
        }
        if bytes.len() > 64 * 1024
        {
            return Err(Error::BadArgument);
        }
        let mut call = ForkWriteCall { operation: APPEND_FORK_WRITE,
                                       handle: self.handle,
                                       fork: fork_code(fork),
                                       ..Default::default() };
        self.client.fork_write(&mut call, "", "", bytes, &mut [])?;
        self.written[index] = end;
        Ok(())
    }

    /// Read back unpublished payload for copy verification, without exposing
    /// the staging file's catalog identity to application code.
    pub fn read_staged(&mut self, fork: Fork, offset: u64, output: &mut [u8]) -> Result<usize>
    {
        if output.len() > 64 * 1024
        {
            return Err(Error::BadArgument);
        }
        let mut call = ForkWriteCall { operation: READ_FORK_WRITE,
                                       handle: self.handle,
                                       fork: fork_code(fork),
                                       offset,
                                       ..Default::default() };
        self.client.fork_write(&mut call, "", "", &[], output)?;
        if call.transferred > output.len()
        {
            return Err(Error::BadAbi);
        }
        Ok(call.transferred)
    }

    pub fn commit_step(&mut self) -> Result<Option<CatalogInfo>>
    {
        let mut call = ForkWriteCall { operation: FINISH_FORK_WRITE,
                                       handle: self.handle,
                                       ..Default::default() };
        self.client.fork_write(&mut call, "", "", &[], &mut [])?;
        match call.completed
        {
            0 => Ok(None),
            1 =>
            {
                self.handle = 0;
                Ok(Some(call.entry.try_into()?))
            }
            _ => Err(Error::BadAbi)
        }
    }

    pub fn abort_step(&mut self) -> Result<ForkAbort>
    {
        let mut call = ForkWriteCall { operation: ABORT_FORK_WRITE,
                                       handle: self.handle,
                                       ..Default::default() };
        self.client.fork_write(&mut call, "", "", &[], &mut [])?;
        match call.completed
        {
            0 => Ok(ForkAbort::Pending),
            1 =>
            {
                self.handle = 0;
                if call.entry.id == 0
                {
                    Ok(ForkAbort::Aborted)
                }
                else
                {
                    Ok(ForkAbort::Published(call.entry.try_into()?))
                }
            }
            _ => Err(Error::BadAbi)
        }
    }

    pub async fn finish(mut self) -> Result<CatalogInfo>
    {
        core::future::poll_fn(|context| {
            match self.commit_step()
            {
                Ok(Some(entry)) => core::task::Poll::Ready(Ok(entry)),
                Ok(None) =>
                {
                    context.waker().wake_by_ref();
                    core::task::Poll::Pending
                }
                Err(error) => core::task::Poll::Ready(Err(error))
            }
        }).await
    }

    pub async fn abort(mut self) -> Result<ForkAbort>
    {
        core::future::poll_fn(|context| {
            match self.abort_step()
            {
                Ok(ForkAbort::Pending) =>
                {
                    context.waker().wake_by_ref();
                    core::task::Poll::Pending
                }
                result => core::task::Poll::Ready(result)
            }
        }).await
    }
}
impl Drop for ForkWriter<'_>
{
    fn drop(&mut self)
    {
        if self.handle != 0
        {
            let mut call = ForkWriteCall { operation: ABANDON_FORK_WRITE,
                                           handle: self.handle,
                                           ..Default::default() };
            let _ = self.client.fork_write(&mut call, "", "", &[], &mut []);
        }
    }
}

impl Files<'_>
{
    /// Publish copied file/folder properties and comment together, preserving
    /// identity, name, hierarchy and both forks. The catalog clock is separate
    /// from the supplied historical object dates.
    pub fn set_properties(self, volume: VolumeId, id: Cnid, value: ForkMetadata<'_>)
                          -> Result<CatalogInfo>
    {
        self.set_properties_at(volume, id, value, self.client.now()?)
    }

    pub fn set_properties_at(self, volume: VolumeId, id: Cnid, value: ForkMetadata<'_>,
                             when: Timestamp)
                             -> Result<CatalogInfo>
    {
        let entry = mac_abi::files::CatalogInfo { flags: value.attributes.bits(),
                                                  finder_info: value.finder_info,
                                                  created: value.created
                                                                .mac_microseconds()
                                                                .ok_or(Error::BadArgument)?,
                                                  modified: value.modified
                                                                 .mac_microseconds()
                                                                 .ok_or(Error::BadArgument)?,
                                                  backup: value.backup
                                                               .map(|date| {
                                                                   date.mac_microseconds()
                                                                       .ok_or(Error::BadArgument)
                                                               })
                                                               .transpose()?
                                                               .unwrap_or(0),
                                                  backup_present: u32::from(value.backup
                                                                                 .is_some()),
                                                  ..Default::default() };
        self.call(FileCall { operation: SET_PROPERTIES,
                             volume: volume.0,
                             cnid: id.0,
                             entry,
                             position: when.mac_microseconds().ok_or(Error::BadArgument)?,
                             ..Default::default() },
                  value.comment)?
            .entry
            .try_into()
    }
}
