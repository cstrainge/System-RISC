use sha2::{Digest, Sha256};

use crate::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForkLayout
{
    pub offset: u64,
    pub length: u64,
    pub hashes: u64,
    pub chunks: u64
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout
{
    pub data:      ForkLayout,
    pub resources: ForkLayout,
    pub total:     u64
}
impl Layout
{
    pub fn new(metadata: &Metadata, data: u64, resources: u64) -> Result<Self>
    {
        metadata.validate()?;
        if metadata.kind != Kind::File && (data != 0 || resources != 0)
        {
            return Err(Error::Invalid);
        }
        let start = METADATA_BYTES;
        let data = fork(start as u64, data)?;
        let resources = fork(end(data)?, resources)?;
        Ok(Self { data,
                  resources,
                  total: end(resources)? })
    }

    pub fn fork(self, kind: Fork) -> ForkLayout
    {
        match kind
        {
            Fork::Data => self.data,
            Fork::Resource => self.resources
        }
    }
}
fn fork(offset: u64, length: u64) -> Result<ForkLayout>
{
    let chunks =
        length / CHUNK_BYTES as u64 + u64::from(!length.is_multiple_of(CHUNK_BYTES as u64));
    let hashes = offset.checked_add(length.min(CHUNK_BYTES as u64)).ok_or(Error::Range)?;
    let result = ForkLayout { offset,
                              length,
                              hashes,
                              chunks };
    end(result)?;
    Ok(result)
}
fn end(fork: ForkLayout) -> Result<u64>
{
    fork.offset
        .checked_add(fork.length)
        .ok_or(Error::Range)?
        .checked_add(fork.chunks.checked_mul(HASH_BYTES).ok_or(Error::Range)?)
        .ok_or(Error::Range)
}
impl ForkLayout
{
    /// Chunk bytes are immediately followed by their hash. Sequential reads
    /// don't seek back and forth between a fork and a distant hash table.
    pub fn chunk(self, index: u64) -> Result<(u64, usize, u64)>
    {
        if index >= self.chunks
        {
            return Err(Error::Range);
        }
        let logical = index.checked_mul(CHUNK_BYTES as u64).ok_or(Error::Range)?;
        let length =
            self.length.checked_sub(logical).ok_or(Error::Range)?.min(CHUNK_BYTES as u64) as usize;
        let offset =
            self.offset
                .checked_add(index.checked_mul(CHUNK_BYTES as u64 + HASH_BYTES)
                                  .ok_or(Error::Range)?)
                .ok_or(Error::Range)?;
        Ok((offset, length, offset.checked_add(length as u64).ok_or(Error::Range)?))
    }
}
#[derive(Clone, Copy)]
pub struct Header
{
    pub(crate) bytes: [u8; HEADER_BYTES]
}
fn put<const N: usize>(data: &mut [u8], at: usize, value: [u8; N])
{
    data[at..at + N].copy_from_slice(&value);
}
fn u16_at(data: &[u8], at: usize) -> u16
{
    u16::from_le_bytes(data[at..at + 2].try_into().unwrap())
}
fn u32_at(data: &[u8], at: usize) -> u32
{
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}
fn u64_at(data: &[u8], at: usize) -> u64
{
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}
impl Header
{
    pub fn encode(metadata: &Metadata, data: u64, resources: u64) -> Result<(Self, Layout)>
    {
        let layout = Layout::new(metadata, data, resources)?;
        let mut b = [0; HEADER_BYTES];
        put(&mut b, 0, *MAGIC);
        put(&mut b, 8, VERSION.to_le_bytes());
        put(&mut b, 12, (HEADER_BYTES as u32).to_le_bytes());
        put(&mut b, 16, (metadata.kind as u32).to_le_bytes());
        put(&mut b, 20, u32::from(metadata.backup.is_some()).to_le_bytes());
        put(&mut b, 24, metadata.catalog_id.to_le_bytes());
        put(&mut b, 28, metadata.next_catalog_id.to_le_bytes());
        put(&mut b, 32, metadata.identity);
        put(&mut b, 48, metadata.volume);
        put(&mut b, 64, metadata.revision.to_le_bytes());
        put(&mut b, 72, metadata.created.to_le_bytes());
        put(&mut b, 80, metadata.modified.to_le_bytes());
        put(&mut b, 88, metadata.backup.unwrap_or(0).to_le_bytes());
        put(&mut b, 96, metadata.finder_info);
        put(&mut b, 128, u32::from(metadata.attributes).to_le_bytes());
        put(&mut b, 132, (metadata.name.len() as u16).to_le_bytes());
        put(&mut b, 134, (metadata.comment.len() as u16).to_le_bytes());
        for (at, value) in [(136, data),
                            (144, resources),
                            (152, layout.data.offset),
                            (160, layout.data.hashes),
                            (168, layout.resources.offset),
                            (176, layout.resources.hashes),
                            (184, layout.total)]
        {
            put(&mut b, at, value.to_le_bytes());
        }
        put(&mut b, 192, (CHUNK_BYTES as u32).to_le_bytes());
        let digest = checksum(&b, metadata.name.as_bytes(), metadata.comment.as_bytes());
        b[200..232].copy_from_slice(&digest);
        Ok((Self { bytes: b }, layout))
    }

    pub fn decode(bytes: [u8; HEADER_BYTES], actual_length: u64) -> Result<Self>
    {
        if &bytes[..8] != MAGIC
        {
            return Err(Error::Corrupt);
        }
        if u32_at(&bytes, 8) != VERSION
        {
            return Err(Error::Version);
        }
        let b = &bytes;
        if u32_at(b, 12) != HEADER_BYTES as u32
           || !(1..=3).contains(&u32_at(b, 16))
           || u32_at(b, 20) > 1
           || u32_at(b, 128) > u16::MAX as u32
           || u16_at(b, 132) as usize > MAX_NAME
           || u16_at(b, 134) as usize > MAX_COMMENT
           || u32_at(b, 192) != CHUNK_BYTES as u32
           || b[196..200] != [0; 4]
           || b[232..] != [0; 24]
           || u32_at(b, 20) == 0 && u64_at(b, 88) != 0
           || u64_at(b, 184) != actual_length
        {
            return Err(Error::Corrupt);
        }
        Ok(Self { bytes })
    }

    pub fn bytes(&self) -> &[u8; HEADER_BYTES]
    {
        &self.bytes
    }

    pub fn metadata_bytes(&self) -> usize
    {
        usize::from(u16_at(&self.bytes, 132)) + usize::from(u16_at(&self.bytes, 134))
    }

    pub fn metadata(&self, text: &[u8]) -> Result<(Metadata, Layout)>
    {
        let b = &self.bytes;
        let name_len = usize::from(u16_at(b, 132));
        if text.len() != self.metadata_bytes()
        {
            return Err(Error::Corrupt);
        }
        let mut zero = *b;
        zero[200..232].fill(0);
        if checksum(&zero, &text[..name_len], &text[name_len..]) != b[200..232]
        {
            return Err(Error::Corrupt);
        }
        fn owned(bytes: &[u8]) -> Result<String>
        {
            let text = core::str::from_utf8(bytes).map_err(|_| Error::Corrupt)?;
            let mut result = String::new();
            result.try_reserve(text.len()).map_err(|_| Error::NoMemory)?;
            result.push_str(text);
            Ok(result)
        }
        let metadata = Metadata { kind:            match u32_at(b, 16)
                                  {
                                      1 => Kind::File,
                                      2 => Kind::Directory,
                                      3 => Kind::Root,
                                      _ => return Err(Error::Corrupt)
                                  },
                                  catalog_id:      u32_at(b, 24),
                                  next_catalog_id: u32_at(b, 28),
                                  identity:        b[32..48].try_into().unwrap(),
                                  volume:          b[48..64].try_into().unwrap(),
                                  revision:        u64_at(b, 64),
                                  created:         u64_at(b, 72),
                                  modified:        u64_at(b, 80),
                                  backup:          (u32_at(b, 20) != 0).then(|| u64_at(b, 88)),
                                  finder_info:     b[96..128].try_into().unwrap(),
                                  attributes:      u32_at(b, 128) as u16,
                                  name:            owned(&text[..name_len])?,
                                  comment:         owned(&text[name_len..])? };
        let layout = Layout::new(&metadata, u64_at(b, 136), u64_at(b, 144))?;
        if [(152, layout.data.offset),
            (160, layout.data.hashes),
            (168, layout.resources.offset),
            (176, layout.resources.hashes),
            (184, layout.total)].iter()
                                .any(|&(at, value)| u64_at(b, at) != value)
        {
            return Err(Error::Corrupt);
        }
        Ok((metadata, layout))
    }
}
fn checksum(header: &[u8], name: &[u8], comment: &[u8]) -> [u8; 32]
{
    let mut hash = Sha256::new();
    hash.update(header);
    hash.update(name);
    hash.update(comment);
    hash.finalize().into()
}
