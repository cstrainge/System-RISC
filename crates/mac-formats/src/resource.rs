//! Classic big-endian resource forks, independent of the code architecture.
use alloc::{collections::{BTreeMap, BTreeSet},
            vec,
            vec::Vec};

use crate::{Error, Result};
pub mod stream;
pub mod index;

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
    let mut groups: BTreeMap<[u8; 4], Vec<&Resource>> = BTreeMap::new();
    let mut keys = BTreeSet::new();
    for resource in resources
    {
        if !keys.insert((resource.kind, resource.id))
        {
            return Err(Error("duplicate resource type and ID"));
        }
        if resource.name.as_ref().is_some_and(|name| name.len() > 255)
        {
            return Err(Error("resource name exceeds 255 bytes"));
        }
        groups.entry(resource.kind).or_default().push(resource);
    }
    if groups.len() > 65535
    {
        return Err(Error("too many resource types"));
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
            return Err(Error("too many resources of one type"));
        }
        types.extend_from_slice(&kind);
        types.extend_from_slice(&((group.len() - 1) as u16).to_be_bytes());
        types.extend_from_slice(
            &u16::try_from(refs_base + refs.len())
                .map_err(|_| Error("resource reference offset exceeds 16 bits"))?
                .to_be_bytes(),
        );
        for resource in group
        {
            refs.extend_from_slice(&resource.id.to_be_bytes());
            let name_offset = if let Some(name) = &resource.name
            {
                let offset = u16::try_from(names.len()).map_err(|_| {
                                                           Error("resource name offset exceeds 16 \
                                                                  bits")
                                                       })?;
                if offset == 65535
                {
                    return Err(Error("resource name offset is reserved"));
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
            if data.len() >= 1 << 24
            {
                return Err(Error("resource data offset exceeds 24 bits"));
            }
            refs.push(resource.attributes);
            refs.extend_from_slice(&(data.len() as u32).to_be_bytes()[1..]);
            refs.extend_from_slice(&[0; 4]);
            data.extend_from_slice(&u32::try_from(resource.data.len()).map_err(|_| {
                                                                          Error("resource exceeds \
                                                                                 32 bits")
                                                                      })?
                                                                      .to_be_bytes());
            data.extend_from_slice(&resource.data);
        }
    }
    let names_offset = u16::try_from(28 + types.len() + refs.len()).map_err(|_| {
                                                                       Error("resource map offset \
                                                                              exceeds 16 bits")
                                                                   })?;
    let mut map = vec![0; 24];
    map.extend_from_slice(&28u16.to_be_bytes());
    map.extend_from_slice(&names_offset.to_be_bytes());
    map.extend(types);
    map.extend(refs);
    map.extend(names);
    let mut output = vec![0; 256];
    let map_offset =
        u32::try_from(256 + data.len()).map_err(|_| Error("resource file exceeds 32 bits"))?;
    for (index, value) in
        [256,
         map_offset,
         data.len() as u32,
         u32::try_from(map.len()).map_err(|_| Error("resource map exceeds 32 bits"))?].iter()
                                                                                      .enumerate()
    {
        output[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
    }
    output.extend(data);
    output.extend(map);
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

#[cfg(test)]
mod tests
{
    use super::*;
    use crate::{be16, be32};

    #[test]
    fn borrowed_resources_share_validated_payload_storage()
    {
        let resource = Resource { kind:       *b"sfnt",
                                  id:         128,
                                  name:       Some(vec![b'T']),
                                  attributes: 0,
                                  data:       vec![0x51; 8192] };
        let fork = encode(&[resource]).unwrap();
        let views = decode_borrowed(&fork).unwrap();
        assert_eq!(views[0].data.len(), 8192);
        let range = fork.as_ptr_range();
        assert!(views[0].data.as_ptr() >= range.start && views[0].data.as_ptr() < range.end);
        assert_eq!(views[0].name, Some(&b"T"[..]));
        assert!(decode_borrowed(&fork[..fork.len() - 1]).is_err());
    }

    #[test]
    fn multiple_types_names_attributes_and_signed_ids()
    {
        let items = vec![Resource { kind:       *b"STR ",
                                    id:         -1,
                                    name:       Some(vec![0x8e, b'x']),
                                    attributes: 0x20,
                                    data:       vec![2, b'H', b'i'] },
                         Resource { kind:       *b"RV64",
                                    id:         0,
                                    name:       None,
                                    attributes: 0,
                                    data:       vec![0x7f, b'E', b'L', b'F'] },];
        let encoded = encode(&items).unwrap();
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded, vec![items[1].clone(), items[0].clone()]);
        assert_eq!(encode(&decoded).unwrap(), encoded);
        for len in 0..encoded.len()
        {
            assert!(decode(&encoded[..len]).is_err(), "accepted prefix {len}");
        }
        let mut duplicate = items.clone();
        duplicate.push(items[0].clone());
        assert!(encode(&duplicate).is_err());
    }

    #[test]
    fn empty_fork_and_corrupt_offsets()
    {
        let empty = encode(&[]).unwrap();
        assert_eq!(decode(&empty).unwrap(), vec![]);
        let mut bad = empty.clone();
        bad[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode(&bad).is_err());
        let mut bad = empty;
        bad[256 + 24..256 + 26].copy_from_slice(&u16::MAX.to_be_bytes());
        assert!(decode(&bad).is_err());
    }

    #[test]
    fn rejects_shared_reference_lists()
    {
        let resource = |kind| {
            Resource { kind,
                       id: 0,
                       name: None,
                       attributes: 0,
                       data: vec![42] }
        };
        let mut bytes = encode(&[resource(*b"AAAA"), resource(*b"BBBB")]).unwrap();
        let map = be32(&bytes, 4).unwrap() as usize;
        let types = map + be16(&bytes, map + 24).unwrap() as usize;
        let first_offset = be16(&bytes, types + 8).unwrap();
        bytes[types + 16..types + 18].copy_from_slice(&first_offset.to_be_bytes());
        assert!(decode(&bytes).is_err());
    }
}
