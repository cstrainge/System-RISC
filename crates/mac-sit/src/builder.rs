//! Header-selected external formats; no filename probing or whole-archive buffer.
use crate::{Error, Index, Limits, ReadRequest, Result, index::ClassicBuilder, v5};
enum Builder
{
    Detect
    {
        length: u64,
        limits: Limits
    },
    Classic(ClassicBuilder),
    Five(v5::Builder)
}
pub struct IndexBuilder
{
    builder: Builder,
    failed:  bool
}
impl IndexBuilder
{
    pub fn new(length: u64, limits: Limits) -> Result<Self>
    {
        if length < crate::ARCHIVE_HEADER_BYTES as u64
        {
            return Err(Error::Truncated);
        }
        Ok(Self { builder: Builder::Detect { length,
                                             limits },
                  failed:  false })
    }

    pub fn request(&self) -> Result<Option<ReadRequest>>
    {
        if self.failed
        {
            return Err(Error::InvalidState);
        }
        match &self.builder
        {
            Builder::Detect { .. } =>
            {
                Ok(Some(ReadRequest { offset: 0,
                                      length: crate::ARCHIVE_HEADER_BYTES }))
            }
            Builder::Classic(builder) => builder.request(),
            Builder::Five(builder) => builder.request()
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<()>
    {
        if self.failed
        {
            return Err(Error::InvalidState);
        }
        let result = self.accept(bytes);
        self.failed = result.is_err();
        result
    }

    fn accept(&mut self, bytes: &[u8]) -> Result<()>
    {
        match &mut self.builder
        {
            Builder::Detect { length,
                              limits } =>
            {
                if bytes.len() != crate::ARCHIVE_HEADER_BYTES
                {
                    return Err(Error::Truncated);
                }
                if bytes.starts_with(b"StuffIt (c)1997-")
                {
                    self.builder = Builder::Five(v5::Builder::new(*length, *limits)?);
                }
                else
                {
                    let mut builder = ClassicBuilder::new(*length, *limits)?;
                    builder.feed(bytes)?;
                    self.builder = Builder::Classic(builder);
                }
                Ok(())
            }
            Builder::Classic(builder) => builder.feed(bytes),
            Builder::Five(builder) => builder.feed(bytes)
        }
    }

    pub fn finish(self) -> Result<Index>
    {
        if self.failed
        {
            return Err(Error::InvalidState);
        }
        match self.builder
        {
            Builder::Classic(builder) => builder.finish(),
            Builder::Five(builder) => builder.finish(),
            Builder::Detect { .. } => Err(Error::InvalidState)
        }
    }
}
