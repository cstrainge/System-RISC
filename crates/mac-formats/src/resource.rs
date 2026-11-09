//! Classic and native big-endian resource forks, independent of code architecture.
use alloc::{collections::{BTreeMap, BTreeSet},
            vec,
            vec::Vec};

use crate::{Error, Result};
pub mod stream;
pub mod index;
pub mod loader;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding
{
    Classic,
    Native
}

pub fn encoding(bytes: &[u8], length: u64) -> Result<Encoding>
{
    Ok(stream::Header::decode(bytes, length)?.encoding)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource
{
    pub kind:       [u8; 4],
    pub id:         i16,
    /// Raw Mac Roman, without a Pascal length byte.
    pub name:       Option<Vec<u8>>,
    pub attributes: u8,
    pub data:       Vec<u8>
}

pub fn encode(resources: &[Resource]) -> Result<Vec<u8>>
{
    encode_as(resources, Encoding::Classic)
}

pub fn encode_native(resources: &[Resource]) -> Result<Vec<u8>>
{
    encode_as(resources, Encoding::Native)
}

fn encode_as(resources: &[Resource], encoding: Encoding) -> Result<Vec<u8>>
{
    let mut groups: BTreeMap<[u8; 4], Vec<&Resource>> = BTreeMap::new();
    let mut keys = BTreeSet::new();
    for resource in resources
    {
        if !keys.insert((resource.kind, resource.id))
        {
            return Err(Error::Malformed("duplicate resource type and ID"));
        }
        if resource.name.as_ref().is_some_and(|name| name.len() > 255)
        {
            return Err(Error::Malformed("resource name exceeds 255 bytes"));
        }
        groups.entry(resource.kind).or_default().push(resource);
    }
    if groups.len() > 65535
    {
        return Err(Error::Malformed("too many resource types"));
    }
    let mut data = Vec::new();
    let mut names = Vec::new();
    let mut types = Vec::new();
    types.extend_from_slice(&(groups.len() as u16).wrapping_sub(1).to_be_bytes());
    let mut refs = Vec::new();
    let refs_base = 2 + 8 * groups.len();
    for (kind, mut group) in groups
    {
        group.sort_by_key(|resource| resource.id);
        if group.len() > 65535
        {
            return Err(Error::Malformed("too many resources of one type"));
        }
        types.extend_from_slice(&kind);
        types.extend_from_slice(&((group.len() - 1) as u16).to_be_bytes());
        types.extend_from_slice(&u16::try_from(refs_base + refs.len()).map_err(|_| {
                                     Error::Malformed("resource reference offset exceeds 16 bits")
                                 })?
                                 .to_be_bytes());
        for resource in group
        {
            refs.extend_from_slice(&resource.id.to_be_bytes());
            let name_offset = if let Some(name) = &resource.name
            {
                let offset = u16::try_from(names.len()).map_err(|_| {
                                                           Error::Malformed("resource name offset \
                                                                             exceeds 16 bits")
                                                       })?;
                if offset == 65535
                {
                    return Err(Error::Malformed("resource name offset is reserved"));
                }
                names.push(name.len() as u8);
                names.extend_from_slice(name);
                offset
            }
            else
            {
                65535
            };
            refs.extend_from_slice(&name_offset.to_be_bytes());
            if encoding == Encoding::Classic && data.len() >= 1 << 24
            {
                return Err(Error::Malformed("resource data offset exceeds 24 bits"));
            }
            refs.push(resource.attributes);
            let offset = u32::try_from(data.len()).map_err(|_| {
                                                      Error::Malformed("resource data offset \
                                                                        exceeds 32 bits")
                                                  })?;
            match encoding
            {
                Encoding::Classic =>
                {
                    refs.extend_from_slice(&offset.to_be_bytes()[1..]);
                    refs.extend_from_slice(&[0; 4]);
                }
                Encoding::Native =>
                {
                    refs.extend_from_slice(&offset.to_be_bytes());
                    refs.extend_from_slice(&[0; 3]);
                }
            }
            data.extend_from_slice(&u32::try_from(resource.data.len()).map_err(|_| {
                                                                          Error::Malformed("resource exceeds \
                                                                                 32 bits")
                                                                      })?
                                                                      .to_be_bytes());
            data.extend_from_slice(&resource.data);
        }
    }
    let names_offset =
        u16::try_from(28 + types.len() + refs.len()).map_err(|_| {
                                                        Error::Malformed("resource map offset \
                                                                          exceeds 16 bits")
                                                    })?;
    let mut map = vec![0; 24];
    map.extend_from_slice(&28u16.to_be_bytes());
    map.extend_from_slice(&names_offset.to_be_bytes());
    map.extend(types);
    map.extend(refs);
    map.extend(names);
    let mut output = vec![0; 256];
    let map_offset = u32::try_from(256 + data.len()).map_err(|_| {
                                                        Error::Malformed("resource file exceeds \
                                                                          32 bits")
                                                    })?;
    for (index, value) in [256,
                           map_offset,
                           data.len() as u32,
                           u32::try_from(map.len()).map_err(|_| {
                                                       Error::Malformed("resource map exceeds 32 \
                                                                         bits")
                                                   })?].iter()
                                                       .enumerate()
    {
        output[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
    }
    output.extend(data);
    output.extend(map);
    u32::try_from(output.len()).map_err(|_| Error::Malformed("resource file exceeds 32 bits"))?;
    if encoding == Encoding::Native
    {
        // Zero is invalid as a classic data offset: these layouts cannot alias.
        // Native data starts at 256 and its map follows the data contiguously.
        output[..4].fill(0);
        output[4..8].copy_from_slice(b"RRF1");
    }
    Ok(output)
}

/// Validated resource views borrow payloads rather than copying a whole font bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceRef<'a>
{
    pub kind:       [u8; 4],
    pub id:         i16,
    pub name:       Option<&'a [u8]>,
    pub attributes: u8,
    pub data:       &'a [u8]
}
pub fn decode(bytes: &[u8]) -> Result<Vec<Resource>>
{
    Ok(decode_borrowed(bytes)?.into_iter()
                              .map(|r| {
                                  Resource { kind:       r.kind,
                                             id:         r.id,
                                             name:       r.name.map(|n| n.to_vec()),
                                             attributes: r.attributes,
                                             data:       r.data.to_vec() }
                              })
                              .collect())
}

pub fn decode_borrowed(bytes: &[u8]) -> Result<Vec<ResourceRef<'_>>>
{
    stream::borrowed(bytes)
}

