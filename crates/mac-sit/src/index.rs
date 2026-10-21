use alloc::vec::Vec;

use crate::*;

#[derive(Clone, Copy, Debug)]
pub struct Limits
{
    pub entries: usize,
    pub depth:   usize
}
impl Default for Limits
{
    fn default() -> Self
    {
        Self { entries: 4096,
               depth:   64 }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadRequest
{
    pub offset: u64,
    pub length: usize
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Contents
{
    File(FileHeader),
    Folder
    {
        header:    FolderHeader,
        top_level: bool
    }
}
/// These are persistent catalog numbers derived from immutable archive order,
/// not runtime handles. Mounted-volume/open/source IDs belong to System's
/// generation facility. The mounted-volume generation scopes these numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node
{
    pub id:          u32,
    pub parent:      u32,
    pub child_count: u32,
    pub contents:    Contents
}
impl Node
{
    pub fn name(&self) -> &Name
    {
        match &self.contents
        {
            Contents::File(file) => &file.name,
            Contents::Folder { header,
                               .. } => &header.name
        }
    }

    pub fn is_directory(&self) -> bool
    {
        matches!(self.contents, Contents::Folder { .. })
    }
}
#[derive(Debug)]
pub struct Index
{
    header: ArchiveHeader,
    nodes:  Vec<Node>
}
impl Index
{
    pub fn header(&self) -> ArchiveHeader
    {
        self.header
    }

    pub fn nodes(&self) -> &[Node]
    {
        &self.nodes
    }

    pub fn get(&self, id: u32) -> Option<&Node>
    {
        let index = usize::try_from(id.checked_sub(FIRST_ENTRY_ID)?).ok()?;
        self.nodes.get(index).filter(|node| node.id == id)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage
{
    Header,
    Entries,
    Done,
    Failed
}
pub struct IndexBuilder
{
    source_length: u64,
    limits:        Limits,
    header:        Option<ArchiveHeader>,
    offset:        u64,
    nodes:         Vec<Node>,
    stack:         Vec<usize>,
    top_level:     u32,
    stage:         Stage
}
impl IndexBuilder
{
    pub fn new(source_length: u64, limits: Limits) -> Result<Self>
    {
        if source_length < ARCHIVE_HEADER_BYTES as u64
        {
            return Err(Error::Truncated);
        }
        Ok(Self { source_length,
                  limits,
                  header: None,
                  offset: 0,
                  nodes: Vec::new(),
                  stack: Vec::new(),
                  top_level: 0,
                  stage: Stage::Header })
    }

    pub fn request(&self) -> Result<Option<ReadRequest>>
    {
        let length = match self.stage
        {
            Stage::Header => ARCHIVE_HEADER_BYTES,
            Stage::Entries => ENTRY_HEADER_BYTES,
            Stage::Done => return Ok(None),
            Stage::Failed => return Err(Error::InvalidState)
        };
        Ok(Some(ReadRequest { offset: self.offset,
                              length }))
    }

    /// Supply precisely the requested header bytes. This performs at most one
    /// catalog step; an async caller retains the builder between source reads.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<()>
    {
        let result = self.accept(bytes);
        if result.is_err()
        {
            self.stage = Stage::Failed;
        }
        result
    }

    fn accept(&mut self, bytes: &[u8]) -> Result<()>
    {
        match self.stage
        {
            Stage::Header =>
            {
                self.header = Some(ArchiveHeader::decode(bytes, self.source_length)?);
                self.offset = ARCHIVE_HEADER_BYTES as u64;
                self.stage = Stage::Entries;
            }
            Stage::Entries =>
            {
                let header = self.header.ok_or(Error::InvalidState)?;
                let entry = EntryHeader::decode(bytes, self.offset, header.length)?;
                self.offset =
                    self.offset.checked_add(ENTRY_HEADER_BYTES as u64).ok_or(Error::Bounds)?;
                if entry == EntryHeader::EndFolder
                {
                    self.stack.pop().ok_or(Error::Hierarchy)?;
                }
                else
                {
                    if self.nodes.len() >= self.limits.entries
                    {
                        return Err(Error::Limit);
                    }
                    let directory = matches!(entry, EntryHeader::Folder(_));
                    if directory && self.stack.len() >= self.limits.depth
                    {
                        return Err(Error::Limit);
                    }
                    self.nodes.try_reserve(1).map_err(|_| Error::Allocation)?;
                    if directory
                    {
                        self.stack.try_reserve(1).map_err(|_| Error::Allocation)?;
                    }
                    let parent = if let Some(index) = self.stack.last()
                    {
                        let parent = self.nodes.get_mut(*index).ok_or(Error::Hierarchy)?;
                        parent.child_count =
                            parent.child_count.checked_add(1).ok_or(Error::Bounds)?;
                        parent.id
                    }
                    else
                    {
                        self.top_level = self.top_level.checked_add(1).ok_or(Error::Bounds)?;
                        ROOT_ID
                    };
                    let id = u32::try_from(self.nodes.len()).map_err(|_| Error::Limit)?
                                                            .checked_add(FIRST_ENTRY_ID)
                                                            .ok_or(Error::Limit)?;
                    let contents = match entry
                    {
                        EntryHeader::File(file) =>
                        {
                            self.offset = file.data.end()?;
                            Contents::File(file)
                        }
                        EntryHeader::Folder(header) =>
                        {
                            Contents::Folder { header,
                                               top_level: self.stack.is_empty() }
                        }
                        EntryHeader::EndFolder => return Err(Error::InvalidState)
                    };
                    if directory
                    {
                        self.stack.push(self.nodes.len());
                    }
                    self.nodes.push(Node { id,
                                           parent,
                                           child_count: 0,
                                           contents });
                }
            }
            Stage::Done | Stage::Failed => return Err(Error::InvalidState)
        }
        let header = self.header.ok_or(Error::InvalidState)?;
        if self.offset == header.length
        {
            if !self.stack.is_empty()
            {
                return Err(Error::Hierarchy);
            }
            if self.top_level != u32::from(header.top_level_count)
            {
                return Err(Error::Count);
            }
            self.stage = Stage::Done;
        }
        else if self.offset
                      .checked_add(ENTRY_HEADER_BYTES as u64)
                      .is_none_or(|end| end > header.length)
        {
            return Err(Error::Truncated);
        }
        Ok(())
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
