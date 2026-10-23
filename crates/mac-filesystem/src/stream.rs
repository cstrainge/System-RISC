//! Format-neutral streaming publication for the resident File Manager. Classic
//! HFS has no native staging contract; that backend rejects admission explicitly.
pub use mac_hfs_b::{Error as NativeError, StreamAbort};

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamError
{
    Unsupported,
    Policy(MutationError),
    Native(NativeError)
}
pub type StreamResult<T> = core::result::Result<T, StreamError>;
impl From<MutationError> for StreamError
{
    fn from(error: MutationError) -> Self
    {
        Self::Policy(error)
    }
}
impl core::fmt::Display for StreamError
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Unsupported =>
            {
                f.write_str("filesystem does not support streamed fork publication")
            }
            Self::Policy(error) => write!(f, "streamed write rejected by file policy: {error:?}"),
            Self::Native(error) => core::fmt::Display::fmt(error, f)
        }
    }
}
impl core::error::Error for StreamError
{
}

pub struct StreamSpec<'a>
{
    pub parent:          u32,
    pub name:            &'a str,
    pub expected:        u32,
    pub data_length:     u64,
    pub resource_length: u64,
    pub flags:           u16,
    pub finder_info:     [u8; 32],
    pub created:         u64,
    pub modified:        u64,
    pub backup:          Option<u64>,
    pub comment:         &'a str
}
#[must_use = "retain staging until committed or abort is acknowledged"]
pub struct StagedFile(mac_hfs_b::StagedFile);

impl<D: WritableBlockDevice> Volume<'_, D>
{
    pub fn begin_stream(&mut self, spec: StreamSpec<'_>, when: u64, protected: &[u32])
                        -> StreamResult<StagedFile>
    {
        let Backend::HfsB(volume) = &mut self.backend
        else
        {
            return Err(StreamError::Unsupported);
        };
        if spec.expected != 0
        {
            mutation::native_protection(volume, spec.expected, protected)?;
        }
        let state = volume.begin_stream(mac_hfs_b::StreamSpec {
            parent: spec.parent, name: spec.name, expected: spec.expected,
            data_length: spec.data_length, resource_length: spec.resource_length,
            properties: mac_hfs_b::StreamProperties {flags:spec.flags, finder_info:spec.finder_info,
                created:Timestamp(spec.created), modified:Timestamp(spec.modified),
                backup:spec.backup.map(Timestamp),comment:spec.comment}
        },Timestamp(when)).map_err(StreamError::Native)?;
        self.refresh();
        Ok(StagedFile(state))
    }

    pub fn append_stream(&mut self, state: &mut StagedFile, resource: bool, data: &[u8], when: u64)
                         -> StreamResult<()>
    {
        let Backend::HfsB(volume) = &mut self.backend
        else
        {
            return Err(StreamError::Unsupported);
        };
        volume.append_stream(&mut state.0,
                             if resource { ForkKind::Resource } else { ForkKind::Data },
                             data,
                             Timestamp(when))
              .map_err(StreamError::Native)?;
        self.refresh();
        Ok(())
    }

    pub fn finish_stream(&mut self, state: &mut StagedFile, when: u64, protected: &[u32])
                         -> StreamResult<CatalogEntry>
    {
        let Backend::HfsB(volume) = &mut self.backend
        else
        {
            return Err(StreamError::Unsupported);
        };
        if let Some(id) = state.0.replacement_id()
        {
            mutation::native_protection(volume, id, protected)?;
        }
        let entry =
            volume.finish_stream(&mut state.0, Timestamp(when)).map_err(StreamError::Native)?;
        self.refresh();
        Ok(entry.into())
    }

    pub fn read_stream(&mut self, state: &StagedFile, resource: bool, offset: u64,
                       output: &mut [u8])
                       -> StreamResult<usize>
    {
        let Backend::HfsB(volume) = &mut self.backend
        else
        {
            return Err(StreamError::Unsupported);
        };
        volume.read_stream(&state.0,
                           if resource { ForkKind::Resource } else { ForkKind::Data },
                           offset,
                           output)
              .map_err(StreamError::Native)
    }

    pub fn abort_stream(&mut self, state: &mut StagedFile, when: u64) -> StreamResult<StreamAbort>
    {
        let Backend::HfsB(volume) = &mut self.backend
        else
        {
            return Err(StreamError::Unsupported);
        };
        let published =
            volume.abort_stream(&mut state.0, Timestamp(when)).map_err(StreamError::Native)?;
        self.refresh();
        Ok(published)
    }

    /// Exclusive boot recovery reclaims one unpublished file per call. Other
    /// filesystem formats have no streams and therefore no work to recover.
    pub fn recover_stream_step(&mut self, when: u64) -> StreamResult<bool>
    {
        let Backend::HfsB(volume) = &mut self.backend
        else
        {
            return Ok(false);
        };
        let changed = volume.recover_stream_step(Timestamp(when)).map_err(StreamError::Native)?;
        self.refresh();
        Ok(changed)
    }
}
