//! Owned resource-map index for seekable readers. Only map/reference metadata is
//! cached; code and other payloads remain in the fork. Parsing is shared with
//! the borrowed and streaming decoders, with fallible bounded allocations.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use super::stream::{self, Budget, Header, Location};
use crate::{Error, Result, text};

#[derive(Clone, Copy, Debug)]
pub struct MapLayout
{
    header:         Header,
    resource_limit: usize
}
impl MapLayout
{
    pub fn decode(header: &[u8; 16], fork_length: u64, budget: Budget) -> Result<Self>
    {
        if fork_length < 16
        {
            return Err(Error::Malformed("truncated resource header"));
        }
        let header = Header::decode(header, fork_length)?;
        if header.map_length > budget.map_bytes
        {
            return Err(Error::BudgetExceeded("resource map exceeds budget"));
        }
        Ok(Self { header,
                  resource_limit: budget.resources })
    }

    pub fn map_offset(self) -> u64
    {
        self.header.map
    }

    pub fn map_length(self) -> usize
    {
        self.header.map_length
    }
}
struct Type
{
    kind:  [u8; 4],
    start: usize,
    end:   usize
}
/// A borrowed map entry. `number` identifies this entry only within its owned
/// index; it is not a live Toolbox object ID. Offset points to the length word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry<'a>
{
    /// Position of this validated reference record within the fork.
    pub reference_offset: u64,
    pub number:           usize,
    pub kind:             [u8; 4],
    pub id:               i16,
    pub attributes:       u8,
    pub name:             Option<&'a [u8]>,
    pub offset:           u64,
    pub data_end:         u64,
    pub length:           Option<u32>
}
pub struct Index
{
    layout:  MapLayout,
    map:     Vec<u8>,
    entries: Vec<Location>,
    by_type: Vec<usize>,
    by_key:  Vec<usize>,
    types:   Vec<Type>,
    lengths: Vec<Option<u32>>
}
fn reserve<T>(values: &mut Vec<T>, count: usize) -> Result<()>
{
    values.try_reserve_exact(count)
          .map_err(|_| Error::AllocationFailed("resource allocation failed"))
}
impl Index
{
    pub fn encoding(&self) -> super::Encoding
    {
        self.layout.header.encoding
    }

    /// A zero-length fork has no resources. This does not accept a malformed
    /// nonempty fork; those must pass MapLayout/decode normally.
    pub fn empty() -> Self
    {
        Self { layout:  MapLayout { header:         Header { encoding:    super::Encoding::Classic,
                                                             data:        0,
                                                             map:         0,
                                                             data_length: 0,
                                                             map_length:  0 },
                                    resource_limit: 0 },
               map:     Vec::new(),
               entries: Vec::new(),
               by_type: Vec::new(),
               by_key:  Vec::new(),
               types:   Vec::new(),
               lengths: Vec::new() }
    }

    pub fn decode(layout: MapLayout, map: Vec<u8>) -> Result<Self>
    {
        if map.len() != layout.header.map_length
        {
            return Err(Error::Malformed("resource map length mismatch"));
        }
        let entries = stream::locations(&map, layout.header.data_length, layout.resource_limit,
                                        layout.header.encoding)?;
        let mut by_type = Vec::new();
        let mut by_key = Vec::new();
        let mut lengths = Vec::new();
        for vec in [&mut by_type, &mut by_key]
        {
            reserve(vec, entries.len())?;
            vec.extend(0..entries.len());
        }
        reserve(&mut lengths, entries.len())?;
        lengths.resize(entries.len(), None);
        // Preserve original per-type reference order for GetIndResource.
        by_type.sort_unstable_by_key(|&i| (entries[i].kind, i));
        by_key.sort_unstable_by_key(|&i| (entries[i].kind, entries[i].id));
        let mut types: Vec<Type> = Vec::new();
        for (position, &index) in by_type.iter().enumerate()
        {
            let kind = entries[index].kind;
            if let Some(last) = types.last_mut()
               && last.kind == kind
            {
                last.end = position + 1;
            }
            else
            {
                types.try_reserve(1)
                     .map_err(|_| Error::AllocationFailed("resource allocation failed"))?;
                types.push(Type { kind,
                                  start: position,
                                  end: position + 1 });
            }
        }
        Ok(Self { layout,
                  map,
                  entries,
                  by_type,
                  by_key,
                  types,
                  lengths })
    }

    pub fn type_count(&self) -> usize
    {
        self.types.len()
    }

    pub fn type_at(&self, index: usize) -> Option<[u8; 4]>
    {
        self.types.get(index).map(|entry| entry.kind)
    }

    fn group(&self, kind: [u8; 4]) -> Option<&Type>
    {
        self.types.binary_search_by_key(&kind, |entry| entry.kind).ok().map(|at| &self.types[at])
    }

    pub fn count(&self, kind: [u8; 4]) -> usize
    {
        self.group(kind).map_or(0, |group| group.end - group.start)
    }

    pub fn entry(&self, number: usize) -> Option<Entry<'_>>
    {
        let entry = self.entries.get(number)?;
        Some(Entry { reference_offset: self.layout.header.map + entry.reference as u64,
                     number,
                     kind: entry.kind,
                     id: entry.id,
                     attributes: entry.attributes,
                     name: entry.name.clone().map(|range| &self.map[range]),
                     offset: self.layout.header.data + entry.offset,
                     data_end: self.layout.header.data + self.layout.header.data_length,
                     length: self.lengths[number] })
    }

    pub fn at(&self, kind: [u8; 4], index: usize) -> Option<Entry<'_>>
    {
        let group = self.group(kind)?;
        if index >= group.end - group.start
        {
            return None;
        }
        self.entry(self.by_type[group.start + index])
    }

    pub fn get(&self, kind: [u8; 4], id: i16) -> Option<Entry<'_>>
    {
        let at =
            self.by_key
                .binary_search_by_key(&(kind, id), |&i| (self.entries[i].kind, self.entries[i].id))
                .ok()?;
        self.entry(self.by_key[at])
    }

    pub fn named(&self, kind: [u8; 4], name: &str) -> Option<Entry<'_>>
    {
        let group = self.group(kind)?;
        self.by_type[group.start..group.end].iter()
                                            .filter_map(|&index| self.entry(index))
                                            .find(|entry| {
                                                entry.name.is_some_and(|bytes| {
                                                              bytes.iter()
                                                                   .map(|&byte| {
                                                                       text::character(byte)
                                                                   })
                                                                   .eq(name.chars())
                                                          })
                                            })
    }

    /// Cache a checked length word after the reader obtains it. Rechecking an
    /// entry cannot silently change the snapshot's interpretation.
    pub fn record_length(&mut self, number: usize, length: u32) -> Result<()>
    {
        let entry = self.entry(number).ok_or(Error::Malformed("unknown resource entry"))?;
        if entry.offset + 4 + u64::from(length) > entry.data_end
        {
            return Err(Error::Malformed("resource payload outside data region"));
        }
        if entry.length.is_some_and(|prior| prior != length)
        {
            return Err(Error::Malformed("resource length changed within open fork"));
        }
        self.lengths[number] = Some(length);
        Ok(())
    }
}

#[cfg(test)]
mod tests
{
    use alloc::vec;

    use super::*;
    use crate::resource::{self, Resource};
    fn fork() -> Vec<u8>
    {
        resource::encode(&[Resource { kind:       *b"CODE",
                                      id:         -1,
                                      attributes: 0x28,
                                      name:       Some(vec![0x8e]),
                                      data:       vec![1, 2, 3, 4] },
                           Resource { kind:       *b"CODE",
                                      id:         3,
                                      attributes: 0xff,
                                      name:       None,
                                      data:       vec![] },
                           Resource { kind:       *b"STR ",
                                      id:         0,
                                      attributes: 0,
                                      name:       Some(vec![]),
                                      data:       vec![0] }]).unwrap()
    }
    fn decode(bytes: &[u8]) -> Index
    {
        let layout = MapLayout::decode(bytes[..16].try_into().unwrap(),
                                       bytes.len() as u64,
                                       Budget::default()).unwrap();
        Index::decode(layout,bytes[layout.map_offset() as usize..][..layout.map_length()].to_vec()).unwrap()
    }
    #[test]
    fn key_lookup_and_reference_order_remain_distinct_and_raw_names_are_lossless()
    {
        let mut bytes = fork();
        let map = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let types =
            map + usize::from(u16::from_be_bytes(bytes[map + 24..map + 26].try_into().unwrap()));
        let refs = types
                   + usize::from(u16::from_be_bytes(bytes[types + 8..types + 10].try_into()
                                                                                .unwrap()));
        let (left, right) = bytes[refs..refs + 24].split_at_mut(12);
        left.swap_with_slice(right);
        let mut index = decode(&bytes);
        assert_eq!(index.type_count(), 2);
        assert_eq!(index.type_at(0), Some(*b"CODE"));
        assert_eq!(index.type_at(1), Some(*b"STR "));
        assert_eq!(index.type_at(2), None);
        assert_eq!(index.count(*b"CODE"), 2);
        assert_eq!(index.count(*b"NONE"), 0);
        assert_eq!(index.at(*b"CODE", 0).unwrap().id, 3);
        assert_eq!(index.at(*b"CODE", 1).unwrap().id, -1);
        assert_eq!(index.at(*b"CODE", 2), None);
        let entry = index.named(*b"CODE", "é").unwrap();
        assert_eq!(entry.id, -1);
        assert_eq!(entry.name, Some(&[0x8e][..]));
        assert_eq!(entry.attributes, 0x28);
        let number = entry.number;
        assert_eq!(index.get(*b"CODE", -1), Some(entry));
        assert_eq!(index.named(*b"CODE", ""), None);
        assert!(index.named(*b"STR ", "").is_some());
        assert_eq!(index.named(*b"CODE", "🦀"), None);
        assert_eq!(index.record_length(number, u32::MAX),
                   Err(Error::Malformed("resource payload outside data region")));
        assert_eq!(index.entry(number).unwrap().length, None);
        index.record_length(number, 4).unwrap();
        index.record_length(number, 4).unwrap();
        assert_eq!(index.entry(number).unwrap().length, Some(4));
        assert_eq!(index.record_length(number, 3),
                   Err(Error::Malformed("resource length changed within open fork")));
        assert_eq!(index.entry(number).unwrap().length, Some(4));
    }
    #[test]
    fn nonempty_forks_require_checked_ranges_map_lengths_and_count_budgets()
    {
        let bytes = fork();
        let header: &[u8; 16] = bytes[..16].try_into().unwrap();
        assert!(MapLayout::decode(header, 15, Budget::default()).is_err());
        assert!(MapLayout::decode(header, bytes.len() as u64 - 1, Budget::default()).is_err());
        assert!(MapLayout::decode(header,
                                  bytes.len() as u64,
                                  Budget { map_bytes: 0,
                                           ..Default::default() }).is_err());
        let layout = MapLayout::decode(header,
                                       bytes.len() as u64,
                                       Budget { resources: 1,
                                                ..Default::default() }).unwrap();
        let map = bytes[layout.map_offset() as usize..][..layout.map_length()].to_vec();
        assert!(Index::decode(layout, map.clone()).is_err());
        let layout = MapLayout::decode(header, bytes.len() as u64, Budget::default()).unwrap();
        assert!(Index::decode(layout, map[..map.len() - 1].to_vec()).is_err());
        let empty = Index::empty();
        assert_eq!(empty.type_count(), 0);
        assert_eq!(empty.get(*b"CODE", 0), None);
    }
}
