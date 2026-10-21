use sha2::{Digest, Sha256};

use crate::*;

/// A metadata view and its owned leased source stay together. A reader cannot
/// accidentally read a different file/revision using this file's fork layout.
pub struct Opened<R>
{
    metadata: Metadata,
    layout:   Layout,
    source:   R
}
impl<R: ReadAt> Opened<R>
{
    pub fn open(mut source: R) -> Result<Self>
    {
        source.validate()?;
        let mut bytes = [0; HEADER_BYTES];
        source.read_exact(0, &mut bytes)?;
        let header = Header::decode(bytes, source.len())?;
        let mut text = [0; MAX_NAME + MAX_COMMENT];
        let length = header.metadata_bytes();
        source.read_exact(HEADER_BYTES as u64, &mut text[..length])?;
        let (metadata, layout) = header.metadata(&text[..length])?;
        let pad = (layout.data.offset - HEADER_BYTES as u64 - length as u64) as usize;
        let mut padding = [0; METADATA_BYTES - HEADER_BYTES];
        source.read_exact(HEADER_BYTES as u64 + length as u64, &mut padding[..pad])?;
        if padding[..pad].iter().any(|&b| b != 0)
        {
            return Err(Error::Corrupt);
        }
        source.validate()?;
        Ok(Self { metadata,
                  layout,
                  source })
    }

    pub fn metadata(&self) -> &Metadata
    {
        &self.metadata
    }

    pub fn layout(&self) -> Layout
    {
        self.layout
    }

    pub fn into_inner(self) -> R
    {
        self.source
    }

    pub fn read(&mut self, kind: Fork, offset: u64, output: &mut [u8]) -> Result<usize>
    {
        if output.is_empty() || offset >= self.layout.fork(kind).length
        {
            return Ok(0);
        }
        self.source.validate()?;
        let count = self.read_chunks(kind, offset, output)?;
        self.source.validate()?;
        Ok(count)
    }

    fn read_chunks(&mut self, kind: Fork, offset: u64, output: &mut [u8]) -> Result<usize>
    {
        let fork = self.layout.fork(kind);
        if self.source.len() != self.layout.total
        {
            return Err(Error::Corrupt);
        }
        let count = (fork.length - offset).min(output.len() as u64) as usize;
        let mut copied = 0;
        let mut buffer = [0; CHUNK_BYTES];
        while copied < count
        {
            let position = offset + copied as u64;
            let chunk = position / CHUNK_BYTES as u64;
            let start = chunk * CHUNK_BYTES as u64;
            let (physical, length, hash) = fork.chunk(chunk)?;
            self.source.read_exact(physical, &mut buffer[..length])?;
            let mut expected = [0; 32];
            self.source.read_exact(hash, &mut expected)?;
            let digest: [u8; 32] = Sha256::digest(&buffer[..length]).into();
            if digest != expected
            {
                return Err(Error::Corrupt);
            }
            let within = (position - start) as usize;
            let take = (length - within).min(count - copied);
            output[copied..copied + take].copy_from_slice(&buffer[within..within + take]);
            copied += take;
        }
        Ok(copied)
    }

    pub fn verify(&mut self) -> Result<()>
    {
        self.source.validate()?;
        let mut buffer = [0; CHUNK_BYTES];
        for kind in [Fork::Data, Fork::Resource]
        {
            let fork = self.layout.fork(kind);
            let mut at = 0;
            while at < fork.length
            {
                let count = self.read_chunks(kind, at, &mut buffer)?;
                if count == 0
                {
                    return Err(Error::Corrupt);
                }
                at += count as u64;
            }
        }
        self.source.validate()
    }
}
/// Ordered chunk writer. A valid header is published last; incomplete staging
/// files cannot be mistaken for a successful container. No fork-sized buffers.
pub struct Writer<W>
{
    target:          W,
    header:          Header,
    layout:          Layout,
    data_chunks:     u64,
    resource_chunks: u64,
    header_written:  bool,
    finished:        bool
}
impl<W: WriteAt> Writer<W>
{
    pub fn begin(mut target: W, metadata: &Metadata, data: u64, resources: u64) -> Result<Self>
    {
        let (header, layout) = Header::encode(metadata, data, resources)?;
        target.set_len(layout.total)?;
        target.write_all(0, &[0; HEADER_BYTES])?;
        target.write_all(HEADER_BYTES as u64, metadata.name.as_bytes())?;
        target.write_all(HEADER_BYTES as u64 + metadata.name.len() as u64,
                         metadata.comment.as_bytes())?;
        let end = HEADER_BYTES as u64 + metadata.name.len() as u64 + metadata.comment.len() as u64;
        target.write_all(end,
                         &[0; METADATA_BYTES - HEADER_BYTES]
                             [..(layout.data.offset - end) as usize])?;
        Ok(Self { target,
                  header,
                  layout,
                  data_chunks: 0,
                  resource_chunks: 0,
                  header_written: false,
                  finished: false })
    }

    pub fn write_chunk(&mut self, kind: Fork, bytes: &[u8]) -> Result<()>
    {
        if self.finished
        {
            return Err(Error::Invalid);
        }
        let fork = self.layout.fork(kind);
        let chunk = match kind
        {
            Fork::Data => self.data_chunks,
            Fork::Resource => self.resource_chunks
        };
        if chunk >= fork.chunks
        {
            return Err(Error::Invalid);
        }
        let (physical, expected, hash) = fork.chunk(chunk)?;
        if bytes.len() != expected
        {
            return Err(Error::Invalid);
        }
        self.target.write_all(physical, bytes)?;
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        self.target.write_all(hash, &digest)?;
        match kind
        {
            Fork::Data => self.data_chunks += 1,
            Fork::Resource => self.resource_chunks += 1
        };
        Ok(())
    }

    pub fn finish(&mut self) -> Result<()>
    {
        if self.finished
        {
            return Ok(());
        }
        if self.data_chunks != self.layout.data.chunks
           || self.resource_chunks != self.layout.resources.chunks
        {
            return Err(Error::Incomplete);
        }
        if !self.header_written
        {
            self.target.write_all(0, self.header.bytes())?;
            self.header_written = true;
        }
        self.target.flush()?;
        self.finished = true;
        Ok(())
    }

    pub fn is_finished(&self) -> bool
    {
        self.finished
    }

    pub fn target(&mut self) -> &mut W
    {
        &mut self.target
    }

    pub fn into_inner(self) -> W
    {
        self.target
    }
}
