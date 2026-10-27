//! Bounded, seekable resource-fork reading. Fork size is not an allocation size.
#![forbid(unsafe_code)]
use alloc::vec::Vec;
use core::ops::Range;

use super::{Encoding, Resource, ResourceRef};
use crate::{Error, Result, be16, be32, slice};

#[derive(Clone, Copy)]
pub struct Budget
{
    pub map_bytes:     usize,
    pub payload_bytes: usize,
    pub resources:     usize
}
impl Default for Budget
{
    fn default() -> Self
    {
        Self { map_bytes:     256 * 1024,
               payload_bytes: 4 * 1024 * 1024,
               resources:     8192 }
    }
}
#[derive(Clone, Copy, Debug)]
pub(super) struct Header
{
    pub encoding:    Encoding,
    pub data:        u64,
    pub map:         u64,
    pub data_length: u64,
    pub map_length:  usize
}
impl Header
{
    pub fn decode(bytes: &[u8], total: u64) -> Result<Self>
    {
        let discriminator = be32(bytes, 0)?;
        let encoding = if discriminator == 0
        {
            if slice(bytes, 4, 4)? != b"RRF1"
            {
                return Err(Error::Malformed("unsupported native resource-fork format; rebuild \
                                             or recreate the file"));
            }
            Encoding::Native
        }
        else
        {
            Encoding::Classic
        };
        let data_length = u64::from(be32(bytes, 8)?);
        let map_length = be32(bytes, 12)? as usize;
        let (data, map) = match encoding
        {
            Encoding::Classic => (u64::from(discriminator), u64::from(be32(bytes, 4)?)),
            Encoding::Native => (256, 256 + data_length)
        };
        let data_end =
            data.checked_add(data_length).ok_or(Error::Malformed("resource range overflow"))?;
        let map_end = map.checked_add(map_length as u64)
                         .ok_or(Error::Malformed("resource range overflow"))?;
        if data < 16
           || map < 16
           || data_end > total
           || map_end > total
           || data < map_end && map < data_end
        {
            return Err(Error::Malformed("invalid or overlapping resource regions"));
        }
        if encoding == Encoding::Native && (map_end != total || total > u64::from(u32::MAX))
        {
            return Err(Error::Malformed("native resource-fork length mismatch"));
        }
        Ok(Self { encoding,
                  data,
                  map,
                  data_length,
                  map_length })
    }
}
pub(super) struct Location
{
    pub reference:  usize,
    pub kind:       [u8; 4],
    pub id:         i16,
    pub attributes: u8,
    pub name:       Option<Range<usize>>,
    pub offset:     u64
}

/// Borrowed, completely parsed map metadata. Enumerating unselected references
/// does not load their payloads. Offsets point to checked in-region length words;
/// a caller reading one must still validate its payload length against data_end.
#[derive(Clone, Copy)]
pub struct ReferenceIndex<'a>
{
    header:    Header,
    map:       &'a [u8],
    locations: &'a [Location]
}
impl<'a> ReferenceIndex<'a>
{
    pub fn entries(self) -> impl ExactSizeIterator<Item = super::index::Entry<'a>> + Clone
    {
        self.locations.iter().enumerate().map(move |(number, location)| {
                                             super::index::Entry { reference_offset:
                                                                       self.header.map
                                                                       + location.reference as u64,
                                                                   number,
                                                                   kind: location.kind,
                                                                   id: location.id,
                                                                   attributes:
                                                                       location.attributes,
                                                                   name: location.name
                                                                                 .clone()
                                                                                 .map(|range| {
                                                                                     &self.map
                                                                                         [range]
                                                                                 }),
                                                                   offset: self.header.data
                                                                           + location.offset,
                                                                   data_end: self.header.data
                                                                             + self.header
                                                                                   .data_length,
                                                                   length: None }
                                         })
    }
}
/// Parse reference/name records once for both borrowed and seekable decoders.
pub(super) fn locations(map: &[u8], data_length: u64, limit: usize, encoding: Encoding)
                        -> Result<Vec<Location>>
{
    let types = be16(map, 24)? as usize;
    let names = be16(map, 26)? as usize;
    if types < 28 || names < 28 || names > map.len()
    {
        return Err(Error::Malformed("invalid resource lists"));
    }
    let count = be16(map, types)?;
    let count = if count == u16::MAX { 0 } else { usize::from(count) + 1 };
    let mut previous_end = types + 2 + count * 8;
    if previous_end > names
    {
        return Err(Error::Malformed("type list overlaps names"));
    }
    let mut out: Vec<Location> = Vec::new();
    let mut kinds: Vec<[u8; 4]> = Vec::new();
    kinds.try_reserve_exact(count)
         .map_err(|_| Error::AllocationFailed("resource allocation failed"))?;
    for index in 0..count
    {
        let at = types + 2 + index * 8;
        let kind: [u8; 4] = slice(map, at, 4)?.try_into().unwrap();
        kinds.push(kind);
        let number = usize::from(be16(map, at + 4)?) + 1;
        if out.len().checked_add(number).is_none_or(|n| n > limit)
        {
            return Err(Error::BudgetExceeded("resource count exceeds budget"));
        }
        let refs = types + usize::from(be16(map, at + 6)?);
        let end =
            refs.checked_add(number * 12).ok_or(Error::Malformed("resource reference overflow"))?;
        if refs < previous_end || end > names
        {
            return Err(Error::Malformed("reference lists overlap or are out of order"));
        }
        previous_end = end;
        out.try_reserve_exact(number)
           .map_err(|_| Error::AllocationFailed("resource allocation failed"))?;
        let first = out.len();
        for (ordinal, record) in
            slice(map, refs, number * 12)?.as_chunks::<12>().0.iter().enumerate()
        {
            if encoding == Encoding::Native && record[9..].iter().any(|&byte| byte != 0)
            {
                return Err(Error::Malformed("native resource reference reserved bytes are \
                                             nonzero"));
            }
            let name_offset = be16(record, 2)?;
            let name = if name_offset == u16::MAX
            {
                None
            }
            else
            {
                let start = names + usize::from(name_offset);
                let len = usize::from(*map.get(start)
                                          .ok_or(Error::Malformed("truncated resource name"))?);
                slice(map, start + 1, len)?;
                Some(start + 1..start + 1 + len)
            };
            let offset = u64::from(match encoding
                                   {
                                       Encoding::Classic =>
                                       {
                                           u32::from_be_bytes([0, record[5], record[6], record[7]])
                                       }
                                       Encoding::Native =>
                                       {
                                           u32::from_be_bytes([record[5], record[6], record[7],
                                                               record[8]])
                                       }
                                   });
            if offset.checked_add(4).is_none_or(|n| n > data_length)
            {
                return Err(Error::Malformed("resource offset outside data region"));
            }
            out.push(Location { reference: refs + ordinal * 12,
                                kind,
                                id: be16(record, 0)? as i16,
                                attributes: record[4],
                                name,
                                offset });
        }
        // Resource IDs need not be sorted on disk. Check with bounded scratch
        // rather than quadratic searches or infallible per-record allocations.
        let mut ids = Vec::new();
        ids.try_reserve_exact(number)
           .map_err(|_| Error::AllocationFailed("resource allocation failed"))?;
        ids.extend(out[first..].iter().map(|r| r.id));
        ids.sort_unstable();
        if ids.windows(2).any(|w| w[0] == w[1])
        {
            return Err(Error::Malformed("duplicate resource type and ID"));
        }
    }
    kinds.sort_unstable();
    if kinds.windows(2).any(|w| w[0] == w[1])
    {
        return Err(Error::Malformed("duplicate resource type list"));
    }
    Ok(out)
}
fn buffer(size: usize) -> Result<Vec<u8>>
{
    let mut out = Vec::new();
    out.try_reserve_exact(size)
       .map_err(|_| Error::AllocationFailed("resource allocation failed"))?;
    out.resize(size, 0);
    Ok(out)
}
pub(super) fn borrowed(bytes: &[u8]) -> Result<Vec<ResourceRef<'_>>>
{
    let h = Header::decode(bytes, bytes.len() as u64)?;
    let map = slice(bytes, h.map as usize, h.map_length)?;
    let locations = locations(map, h.data_length, usize::MAX, h.encoding)?;
    let mut out = Vec::new();
    out.try_reserve_exact(locations.len())
       .map_err(|_| Error::AllocationFailed("resource allocation failed"))?;
    for r in locations
    {
        let at = h.data + r.offset;
        let size = u64::from(be32(bytes, at as usize)?);
        if r.offset + 4 + size > h.data_length
        {
            return Err(Error::Malformed("resource payload outside data region"));
        }
        out.push(ResourceRef { kind:       r.kind,
                               id:         r.id,
                               attributes: r.attributes,
                               name:       r.name.map(|n| &map[n]),
                               data:       slice(bytes, (at + 4) as usize, size as usize)? });
    }
    Ok(out)
}
/// Read only selected payloads. Every map record is validated, including keys
/// and names of unselected resources. Preserve names and attributes exactly.
pub fn read_selected(length: u64, budget: Budget, mut select: impl FnMut([u8; 4], i16) -> bool,
                     mut read: impl FnMut(u64, &mut [u8]) -> Result<()>)
                     -> Result<Vec<Resource>>
{
    read_selected_with_index(length, budget, &mut select, |_, _| Ok(()), &mut read)
}

/// Validate complete package framing before admitting any selected payload.
/// The validator can inspect every reference, and use bounded caller-owned
/// scratch to read required headers or small contract metadata. Its reads and
/// allocations must impose their own limits; payload_bytes bounds the later
/// selected projection. No map borrow escapes into executable module entry.
pub fn read_selected_with_index(length: u64, budget: Budget,
                                mut select: impl FnMut([u8; 4], i16) -> bool,
                                validate: impl FnOnce(ReferenceIndex<'_>,
                                       &mut dyn FnMut(u64, &mut [u8]) -> Result<()>)
                                       -> Result<()>,
                                mut read: impl FnMut(u64, &mut [u8]) -> Result<()>)
                                -> Result<Vec<Resource>>
{
    let mut header = [0; 16];
    if length < 16
    {
        return Err(Error::Malformed("truncated resource header"));
    }
    read(0, &mut header)?;
    let h = Header::decode(&header, length)?;
    if h.map_length > budget.map_bytes
    {
        return Err(Error::BudgetExceeded("resource map exceeds budget"));
    }
    let mut map = buffer(h.map_length)?;
    read(h.map, &mut map)?;
    let locations = locations(&map, h.data_length, budget.resources, h.encoding)?;
    validate(ReferenceIndex { header:    h,
                              map:       &map,
                              locations: &locations },
             &mut read)?;
    let mut out = Vec::new();
    let mut remaining = budget.payload_bytes;
    let mut remaining_names = budget.map_bytes;
    for r in locations
    {
        if !select(r.kind, r.id)
        {
            continue;
        }
        let mut word = [0; 4];
        read(h.data + r.offset, &mut word)?;
        let size = u32::from_be_bytes(word) as usize;
        if r.offset + 4 + size as u64 > h.data_length
        {
            return Err(Error::Malformed("resource payload outside data region"));
        }
        let name_length = r.name.as_ref().map_or(0, |n| n.len());
        remaining = remaining.checked_sub(size)
                             .ok_or(Error::BudgetExceeded("resource payload exceeds budget"))?;
        remaining_names =
            remaining_names.checked_sub(name_length)
                           .ok_or(Error::BudgetExceeded("resource names exceed budget"))?;
        let name = if let Some(range) = r.name
        {
            let mut name = buffer(range.len())?;
            name.copy_from_slice(&map[range]);
            Some(name)
        }
        else
        {
            None
        };
        let mut data = buffer(size)?;
        // Keep requests bounded even when one selected code/font is large.
        for (index, chunk) in data.chunks_mut(64 * 1024).enumerate()
        {
            read(h.data + r.offset + 4 + (index * 64 * 1024) as u64, chunk)?;
        }
        out.try_reserve(1).map_err(|_| Error::AllocationFailed("resource allocation failed"))?;
        out.push(Resource { kind: r.kind,
                            id: r.id,
                            attributes: r.attributes,
                            name,
                            data });
    }
    Ok(out)
}
