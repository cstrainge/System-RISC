use alloc::{string::String, vec::Vec};

use mac_driver::{Error, Result};
use mac_filesystem_interface::{self as fs, Metadata};
use mac_hfs_b::names;
use mac_sit::{Contents, Index, ROOT_ID};

use crate::{buffer, format_error};

pub(super) fn name_error(error: mac_hfs_b::Error) -> Error
{
    Error(if error == mac_hfs_b::Error::NoMemory
          {
              mac_abi::NO_MEMORY
          }
          else
          {
              mac_abi::BAD_ARGUMENT
          })
}
pub(super) struct Root
{
    pub name:     String,
    pub created:  u64,
    pub modified: u64
}
impl Root
{
    pub fn decode(bytes: &[u8]) -> Result<Self>
    {
        let m = Metadata::decode(bytes).map_err(|_| Error(mac_abi::BAD_ARGUMENT))?;
        if m.directory
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        Ok(Self { name:     names::normalize(m.name).map_err(name_error)?,
                  created:  m.created,
                  modified: m.modified })
    }
}
struct Name
{
    text:    String,
    key:     String,
    comment: String
}
pub(super) struct Volume
{
    pub token:  u32,
    pub source: u32,
    pub index:  Index,
    root:       Root,
    names:      Vec<Name>,
    // Sorted by (parent, folded name), built once. Scans retain archive order.
    lookup:     Vec<usize>,
    children:   Vec<Vec<u32>>
}
impl Volume
{
    pub fn new(token: u32, source: u32, index: Index, root: Root) -> Result<Self>
    {
        let count = index.nodes().len();
        let mut names = Vec::new();
        let mut lookup = Vec::new();
        let mut children = Vec::new();
        names.try_reserve_exact(count).map_err(|_| Error(mac_abi::NO_MEMORY))?;
        lookup.try_reserve_exact(count).map_err(|_| Error(mac_abi::NO_MEMORY))?;
        children.try_reserve_exact(count + 1).map_err(|_| Error(mac_abi::NO_MEMORY))?;
        for _ in 0..=count
        {
            children.push(Vec::new());
        }
        for (i, node) in index.nodes().iter().enumerate()
        {
            let mut utf8 = [0; 765];
            let text = names::normalize(node.name().utf8(&mut utf8).map_err(format_error)?)
                             .map_err(name_error)?;
            let key = names::comparison(&text).map_err(name_error)?;
            let mut comment = String::new();
            comment.try_reserve(node.comment
                                    .len()
                                    .checked_mul(3)
                                    .ok_or(Error(mac_abi::NO_MEMORY))?)
                   .map_err(|_| Error(mac_abi::NO_MEMORY))?;
            for byte in &node.comment
            {
                comment.push(mac_text::character(*byte));
            }
            if comment.len() > 255
            {
                return Err(Error(mac_abi::archive::ArchiveError::Limit.status()));
            }
            names.push(Name { text,
                              key,
                              comment });
            lookup.push(i);
            let parent = if node.parent == ROOT_ID
            {
                0
            }
            else
            {
                (node.parent - mac_sit::FIRST_ENTRY_ID) as usize + 1
            };
            let list = children.get_mut(parent).ok_or(Error(mac_abi::IO_ERROR))?;
            list.try_reserve(1).map_err(|_| Error(mac_abi::NO_MEMORY))?;
            list.push(node.id);
        }
        lookup.sort_unstable_by(|a, b| {
                  (index.nodes()[*a].parent, &names[*a].key).cmp(&(index.nodes()[*b].parent,
                                                                   &names[*b].key))
              });
        if lookup.windows(2).any(|pair| {
                                index.nodes()[pair[0]].parent == index.nodes()[pair[1]].parent
                                && names[pair[0]].key == names[pair[1]].key
                            })
        {
            return Err(Error(mac_abi::ALREADY_EXISTS));
        }
        Ok(Self { token,
                  source,
                  index,
                  root,
                  names,
                  lookup,
                  children })
    }

    pub fn lookup(&self, parent: u32, name: &str) -> Result<u32>
    {
        self.directory(parent)?;
        let name = names::normalize(name).map_err(name_error)?;
        let key = names::comparison(&name).map_err(name_error)?;
        let at = self.lookup
                     .binary_search_by(|i| {
                         (self.index.nodes()[*i].parent, self.names[*i].key.as_str())
                                                   .cmp(&(parent, key.as_str()))
                     })
                     .map_err(|_| Error(mac_abi::NOT_FOUND))?;
        Ok(self.index.nodes()[self.lookup[at]].id)
    }

    pub fn directory(&self, id: u32) -> Result<&[u32]>
    {
        if id == ROOT_ID
        {
            return Ok(&self.children[0]);
        }
        let node = self.index.get(id).ok_or(Error(mac_abi::NOT_FOUND))?;
        if !node.is_directory()
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        Ok(&self.children[(id - mac_sit::FIRST_ENTRY_ID) as usize + 1])
    }

    pub fn metadata(&self, id: u32) -> Result<Metadata<'_>>
    {
        let mut m = Metadata { id,
                               parent: 0,
                               directory: true,
                               data_length: 0,
                               resource_length: 0,
                               created: self.root.created,
                               modified: self.root.modified,
                               backup: None,
                               child_count: self.index.header().top_level_count.into(),
                               attributes: 0,
                               finder_info: [0; 32],
                               revision: 1,
                               name: &self.root.name,
                               comment: "" };
        if id == ROOT_ID
        {
            return Ok(m);
        }
        let node = self.index.get(id).ok_or(Error(mac_abi::NOT_FOUND))?;
        m.parent = node.parent;
        m.child_count = node.child_count;
        let name = &self.names[(id - mac_sit::FIRST_ENTRY_ID) as usize];
        m.name = &name.text;
        m.comment = &name.comment;
        m.attributes = node.attributes;
        match node.contents
        {
            Contents::File(f) =>
            {
                m.directory = false;
                m.data_length = f.data.length;
                m.resource_length = f.resource.length;
                m.created = mac_sit::mac_microseconds(f.created_seconds).map_err(format_error)?;
                m.modified = mac_sit::mac_microseconds(f.modified_seconds).map_err(format_error)?;
                m.finder_info[..4].copy_from_slice(&f.file_type);
                m.finder_info[4..8].copy_from_slice(&f.creator);
                m.finder_info[8..10].copy_from_slice(&f.finder_flags.to_be_bytes());
            }
            Contents::Folder { header,
                               metadata_valid }
                if metadata_valid =>
            {
                m.created =
                    mac_sit::mac_microseconds(header.created_seconds).map_err(format_error)?;
                m.modified =
                    mac_sit::mac_microseconds(header.modified_seconds).map_err(format_error)?;
                m.finder_info[8..10].copy_from_slice(&header.finder_flags.to_be_bytes());
            }
            // Nested folder fields are unspecified in 1.5.1. Inherit source
            // timestamps rather than interpreting arbitrary bytes as dates.
            _ => ()
        }
        if let Some(info) = node.finder_info
        {
            m.finder_info = info;
        }
        Ok(m)
    }

    pub fn encode(&self, node: u32) -> Result<Vec<u8>>
    {
        let mut output = buffer(fs::MAX_METADATA_BYTES)?;
        let length =
            self.metadata(node)?.encode(&mut output).map_err(|_| Error(mac_abi::BAD_ABI))?;
        output.truncate(length);
        Ok(output)
    }

    pub fn descriptor(&self) -> Result<Vec<u8>>
    {
        let mut output = buffer(fs::MAX_MOUNT_BYTES)?;
        let length =
            fs::Mount { capabilities:       fs::READ,
                        filesystem:         *b"SIT ",
                        filesystem_version: self.index.header().format.version(),
                        capacity:           None,
                        root:               self.metadata(ROOT_ID)? }.encode(&mut output)
                                                                     .map_err(|_| {
                                                                         Error(mac_abi::BAD_ABI)
                                                                     })?;
        output.truncate(length);
        Ok(output)
    }
}
