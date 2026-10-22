//! StuffIt 5's two-block catalog. Layout provenance: XADMaster's
//! XADStuffIt5Parser format notes, independently checked against real archives.
//! Reads are at most 4 KiB; headers are CRC checked before publishing any node.
use alloc::vec::Vec;

use crate::*;
const HEADER: usize = 100;
const PREFIX: usize = 48;
const CHUNK: usize = 4096;
const MAX_ARCHIVE_HEADER: u64 = 1024 * 1024;
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage
{
    Header,
    ArchiveExtra,
    Prefix,
    EntryExtra,
    Metadata,
    Resource,
    Done
}
struct Directory
{
    offset:   u64,
    index:    usize,
    children: u32,
    depth:    usize
}
pub(crate) struct Builder
{
    source_length:   u64,
    limits:          Limits,
    header:          Option<ArchiveHeader>,
    stage:           Stage,
    offset:          u64,
    archive_crc:     Crc16,
    expected_crc:    u16,
    entry_offset:    u64,
    entry:           Vec<u8>,
    metadata:        [u8; 36],
    metadata_length: usize,
    nodes:           Vec<Node>,
    directories:     Vec<Directory>,
    expected_nodes:  usize,
    roots:           u32,
    records:         usize
}
fn word(bytes: &[u8], at: usize) -> Result<u16>
{
    Ok(u16::from_be_bytes(bytes.get(at..at + 2)
                               .ok_or(Error::Truncated)?
                               .try_into()
                               .map_err(|_| Error::Truncated)?))
}
fn long(bytes: &[u8], at: usize) -> Result<u32>
{
    Ok(u32::from_be_bytes(bytes.get(at..at + 4)
                               .ok_or(Error::Truncated)?
                               .try_into()
                               .map_err(|_| Error::Truncated)?))
}
impl Builder
{
    pub fn new(source_length: u64, limits: Limits) -> Result<Self>
    {
        if source_length < HEADER as u64
        {
            return Err(Error::Truncated);
        }
        Ok(Self { source_length,
                  limits,
                  header: None,
                  stage: Stage::Header,
                  offset: 0,
                  archive_crc: Crc16::new(),
                  expected_crc: 0,
                  entry_offset: 0,
                  entry: Vec::new(),
                  metadata: [0; 36],
                  metadata_length: 0,
                  nodes: Vec::new(),
                  directories: Vec::new(),
                  expected_nodes: 0,
                  roots: 0,
                  records: 0 })
    }

    pub fn request(&self) -> Result<Option<ReadRequest>>
    {
        let length = match self.stage
        {
            Stage::Header => HEADER,
            Stage::ArchiveExtra =>
            {
                (self.header.ok_or(Error::InvalidState)?.first_entry - self.offset).min(CHUNK
                                                                                        as u64)
                as usize
            }
            Stage::Prefix => PREFIX,
            Stage::EntryExtra => (word(&self.entry, 6)? as usize - self.entry.len()).min(CHUNK),
            Stage::Metadata => self.metadata_length,
            Stage::Resource => 14,
            Stage::Done => return Ok(None)
        };
        let limit = self.header.map_or(self.source_length, |h| h.length);
        if self.offset.checked_add(length as u64).is_none_or(|end| end > limit)
        {
            return Err(Error::Truncated);
        }
        Ok(Some(ReadRequest { offset: self.offset,
                              length }))
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<()>
    {
        let request = self.request()?.ok_or(Error::InvalidState)?;
        if request.length != bytes.len()
        {
            return Err(Error::Truncated);
        }
        self.offset = self.offset.checked_add(bytes.len() as u64).ok_or(Error::Bounds)?;
        match self.stage
        {
            Stage::Header => self.archive(bytes)?,
            Stage::ArchiveExtra =>
            {
                self.archive_crc.update(bytes);
                if self.offset == self.header.ok_or(Error::InvalidState)?.first_entry
                {
                    self.archive_ready()?;
                }
            }
            Stage::Prefix =>
            {
                if bytes[..4] != [0xa5; 4]
                {
                    return Err(Error::Bounds);
                }
                if !matches!(bytes[4], 1..=3)
                {
                    return Err(Error::UnsupportedEntryVersion(bytes[4]));
                }
                let length = word(bytes, 6)? as usize;
                if length < PREFIX
                {
                    return Err(Error::Bounds);
                }
                self.entry.clear();
                self.entry.try_reserve_exact(length).map_err(|_| Error::Allocation)?;
                self.entry.extend_from_slice(bytes);
                self.entry_offset = request.offset;
                self.stage = Stage::EntryExtra;
                if length == PREFIX
                {
                    self.entry_ready()?;
                }
            }
            Stage::EntryExtra =>
            {
                self.entry.extend_from_slice(bytes);
                if self.entry.len() == word(&self.entry, 6)? as usize
                {
                    self.entry_ready()?;
                }
            }
            Stage::Metadata =>
            {
                self.metadata.fill(0);
                self.metadata[..bytes.len()].copy_from_slice(bytes);
                if word(bytes, 0)? & 1 != 0
                {
                    self.stage = Stage::Resource;
                }
                else
                {
                    self.publish(None)?;
                }
            }
            Stage::Resource => self.publish(Some(bytes))?,
            Stage::Done => return Err(Error::InvalidState)
        }
        Ok(())
    }

    fn archive(&mut self, bytes: &[u8]) -> Result<()>
    {
        if !bytes.starts_with(b"StuffIt (c)1997-")
           || bytes[20..78] != *b" Aladdin Systems, Inc., http://www.aladdinsys.com/StuffIt/"
           || !matches!(&bytes[78..80], b"\r\n" | b"\n\n")
        {
            return Err(Error::Signature);
        }
        if bytes[82] != 5
        {
            return Err(Error::UnsupportedVersion(bytes[82]));
        }
        let length = long(bytes, 84)? as u64;
        let first_entry = long(bytes, 94)? as u64;
        if length < HEADER as u64
           || length > self.source_length
           || first_entry < HEADER as u64
           || first_entry > length
        {
            return Err(Error::Bounds);
        }
        if first_entry > MAX_ARCHIVE_HEADER
        {
            return Err(Error::Limit);
        }
        self.expected_nodes = word(bytes, 92)? as usize;
        if self.expected_nodes > self.limits.entries
        {
            return Err(Error::Limit);
        }
        self.expected_crc = word(bytes, 98)?;
        self.archive_crc.update(&bytes[..98]);
        self.archive_crc.update(&[0, 0]);
        self.header = Some(ArchiveHeader { top_level_count: self.expected_nodes as u16,
                                           length,
                                           first_entry,
                                           format: ArchiveFormat::StuffIt5 });
        self.stage = Stage::ArchiveExtra;
        if first_entry == HEADER as u64
        {
            self.archive_ready()?;
        }
        Ok(())
    }

    fn archive_ready(&mut self) -> Result<()>
    {
        if self.archive_crc.value() != self.expected_crc
        {
            return Err(Error::HeaderChecksum);
        }
        self.next()
    }

    fn next(&mut self) -> Result<()>
    {
        if self.nodes.len() == self.expected_nodes
        {
            let header = self.header.ok_or(Error::InvalidState)?;
            if self.roots != header.top_level_count as u32
               || self.directories.iter().any(|d| self.nodes[d.index].child_count != d.children)
            {
                return Err(Error::Count);
            }
            self.stage = Stage::Done;
        }
        else
        {
            self.stage = Stage::Prefix;
        }
        Ok(())
    }

    fn entry_ready(&mut self) -> Result<()>
    {
        let mut crc = Crc16::new();
        crc.update(&self.entry[..32]);
        crc.update(&[0, 0]);
        crc.update(&self.entry[34..]);
        if crc.value() != word(&self.entry, 32)?
        {
            return Err(Error::HeaderChecksum);
        }
        self.records = self.records.checked_add(1).ok_or(Error::Limit)?;
        if self.records > self.limits.entries.saturating_mul(2).saturating_add(1)
        {
            return Err(Error::Limit);
        }
        if self.entry[9] & 0x40 != 0 && long(&self.entry, 34)? == u32::MAX
        {
            // Synthetic directory sentinels have no name or metadata block.
            return self.next();
        }
        self.metadata_length = if self.entry[4] == 1 { 36 } else { 32 };
        self.stage = Stage::Metadata;
        Ok(())
    }

    fn publish(&mut self, resource_header: Option<&[u8]>) -> Result<()>
    {
        let header = self.header.ok_or(Error::InvalidState)?;
        let flags = self.entry[9];
        let directory = flags & 0x40 != 0;
        let password = if directory { 0 } else { self.entry[47] as usize };
        if flags & 0x20 == 0 && password != 0
        {
            return Err(Error::Bounds);
        }
        let name_start = PREFIX + password;
        let name_end =
            name_start.checked_add(word(&self.entry, 30)? as usize).ok_or(Error::Bounds)?;
        let name = Name::from_macroman(self.entry.get(name_start..name_end).ok_or(Error::Name)?)?;
        let mut comment = Vec::new();
        if name_end < self.entry.len()
        {
            let length = word(&self.entry, name_end)? as usize;
            let start = name_end.checked_add(4).ok_or(Error::Bounds)?;
            let end = start.checked_add(length).ok_or(Error::Bounds)?;
            if end != self.entry.len()
            {
                return Err(Error::Bounds);
            }
            comment.try_reserve_exact(length).map_err(|_| Error::Allocation)?;
            comment.extend_from_slice(self.entry.get(start..end).ok_or(Error::Truncated)?);
        }
        let parent_offset = long(&self.entry, 26)? as u64;
        let (parent, depth) = if parent_offset == 0
        {
            self.roots = self.roots.checked_add(1).ok_or(Error::Count)?;
            (ROOT_ID, 0)
        }
        else
        {
            let at = self.directories
                         .binary_search_by_key(&parent_offset, |d| d.offset)
                         .map_err(|_| Error::Hierarchy)?;
            let d = &self.directories[at];
            let parent = self.nodes.get_mut(d.index).ok_or(Error::Hierarchy)?;
            parent.child_count = parent.child_count.checked_add(1).ok_or(Error::Count)?;
            if parent.child_count > d.children
            {
                return Err(Error::Count);
            }
            (parent.id, d.depth + 1)
        };
        if depth > self.limits.depth
           || directory && depth == self.limits.depth
           || self.nodes.len() >= self.limits.entries
        {
            return Err(Error::Limit);
        }
        let id =
            FIRST_ENTRY_ID.checked_add(u32::try_from(self.nodes.len()).map_err(|_| Error::Limit)?)
                          .ok_or(Error::Limit)?;
        let mut finder_info = [0; 32];
        finder_info[..self.metadata_length - 4].copy_from_slice(&self.metadata
                                                                    [4..self.metadata_length]);
        let finder_flags = word(&self.metadata, 12)?;
        let created_seconds = long(&self.entry, 10)?;
        let modified_seconds = long(&self.entry, 14)?;
        let contents = if directory
        {
            let children = word(&self.entry, 46)? as u32;
            self.expected_nodes =
                self.expected_nodes.checked_add(children as usize).ok_or(Error::Limit)?;
            if self.expected_nodes > self.limits.entries
            {
                return Err(Error::Limit);
            }
            self.directories.try_reserve(1).map_err(|_| Error::Allocation)?;
            self.directories.push(Directory { offset: self.entry_offset,
                                              index: self.nodes.len(),
                                              children,
                                              depth });
            Contents::Folder { header:         FolderHeader { name,
                                                              finder_flags,
                                                              created_seconds,
                                                              modified_seconds },
                               metadata_valid: true }
        }
        else
        {
            let (length, packed_length, crc, method_byte) = if let Some(bytes) = resource_header
            {
                if flags & 0x20 == 0 && bytes[13] != 0
                {
                    return Err(Error::Bounds);
                }
                // Password-protected entries remain listable, but extraction
                // is rejected explicitly until a password service exists.
                if bytes[13] != 0
                {
                    self.offset = self.offset.checked_add(bytes[13] as u64).ok_or(Error::Bounds)?;
                }
                (long(bytes, 0)? as u64, long(bytes, 4)? as u64, word(bytes, 8)?, bytes[12])
            }
            else
            {
                (0, 0, 0, 0)
            };
            let encryption = if flags & 0x20 != 0 { 0x80 } else { 0 };
            let resource = Fork { offset: self.offset,
                                  length,
                                  packed_length,
                                  crc,
                                  method_byte: method_byte | encryption,
                                  format: header.format };
            let data = Fork { offset:        resource.end()?,
                              length:        long(&self.entry, 34)? as u64,
                              packed_length: long(&self.entry, 38)? as u64,
                              crc:           word(&self.entry, 42)?,
                              method_byte:   self.entry[46] | encryption,
                              format:        header.format };
            if data.end()? > header.length
            {
                return Err(Error::Bounds);
            }
            self.offset = data.end()?;
            Contents::File(FileHeader { name,
                                        file_type:
                                            self.metadata[4..8].try_into()
                                                               .map_err(|_| Error::Bounds)?,
                                        creator:
                                            self.metadata[8..12].try_into()
                                                                .map_err(|_| Error::Bounds)?,
                                        finder_flags,
                                        created_seconds,
                                        modified_seconds,
                                        resource,
                                        data })
        };
        self.nodes.try_reserve(1).map_err(|_| Error::Allocation)?;
        self.nodes.push(Node { id,
                               parent,
                               child_count: 0,
                               contents,
                               finder_info: Some(finder_info),
                               attributes: u16::from(flags & 0x80 != 0),
                               comment });
        self.next()
    }

    pub fn finish(self) -> Result<Index>
    {
        if self.stage != Stage::Done
        {
            return Err(Error::InvalidState);
        }
        Ok(Index { header: self.header.ok_or(Error::InvalidState)?,
                   nodes:  self.nodes })
    }
}
