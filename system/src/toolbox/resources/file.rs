//! Bounded file-backed metadata without changing the resident resource chain.
use alloc::{collections::BTreeSet, vec, vec::Vec};

use mac_abi::{resources::*, *};
use mac_formats::resource::Resource;

fn word(bytes: &[u8], at: usize) -> Result<u16, Status>
{
    Ok(u16::from_be_bytes(bytes.get(at..at + 2)
                               .ok_or(BAD_ARGUMENT)?
                               .try_into()
                               .map_err(|_| BAD_ARGUMENT)?))
}
fn long(bytes: &[u8], at: usize) -> Result<u32, Status>
{
    Ok(u32::from_be_bytes(bytes.get(at..at + 4)
                               .ok_or(BAD_ARGUMENT)?
                               .try_into()
                               .map_err(|_| BAD_ARGUMENT)?))
}
fn bounded(start: u64, size: u64, total: u64) -> Result<(), Status>
{
    if start.checked_add(size).is_none_or(|end| end > total) { Err(BAD_ARGUMENT) } else { Ok(()) }
}
/// Only the map and selected small resource are allocated. This path is for
/// metadata, never for loading RV64 code or the application's whole resource fork.
pub(crate) fn read_value(length: u64, key: ResourceKey,
                         mut read: impl FnMut(u64, &mut [u8]) -> Result<(), Status>)
                         -> Result<Vec<u8>, Status>
{
    read_selected(length, |candidate| candidate == key, 518, &mut read)?.pop()
                                                                        .map(|r| r.data)
                                                                        .ok_or(NOT_FOUND)
}

/// Bound both the map and total selected payload; never read executable data.
pub(crate) fn read_selected(length: u64, mut select: impl FnMut(ResourceKey) -> bool,
                            limit: usize,
                            mut read: impl FnMut(u64, &mut [u8]) -> Result<(), Status>)
                            -> Result<Vec<Resource>, Status>
{
    if length == 0
    {
        return Err(NOT_FOUND);
    }
    bounded(0, 16, length)?;
    let mut header = [0; 16];
    read(0, &mut header)?;
    let data = u64::from(long(&header, 0)?);
    let map_at = u64::from(long(&header, 4)?);
    let data_len = u64::from(long(&header, 8)?);
    let map_len = u64::from(long(&header, 12)?);
    bounded(data, data_len, length)?;
    bounded(map_at, map_len, length)?;
    if data < 16 || map_at < 16 || data < map_at + map_len && map_at < data + data_len
    {
        return Err(BAD_ARGUMENT);
    }
    // Classic reference/name offsets are16-bit; allow larger trailing name data
    // within an explicit metadata allocation limit.
    if map_len > 256 * 1024
    {
        return Err(NO_MEMORY);
    }
    let mut map = vec![0; map_len as usize];
    read(map_at, &mut map)?;
    let types = usize::from(word(&map, 24)?);
    let names = usize::from(word(&map, 26)?);
    if types < 28 || names < 28 || names > map.len()
    {
        return Err(BAD_ARGUMENT);
    }
    let count = word(&map, types)?;
    let count = if count == u16::MAX { 0 } else { usize::from(count) + 1 };
    let mut previous_end = types + 2 + count * 8;
    if previous_end > names
    {
        return Err(BAD_ARGUMENT);
    }
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for index in 0..count
    {
        let at = types + 2 + index * 8;
        let kind: [u8; 4] =
            map.get(at..at + 4).ok_or(BAD_ARGUMENT)?.try_into().map_err(|_| BAD_ARGUMENT)?;
        let number = usize::from(word(&map, at + 4)?) + 1;
        let refs = types + usize::from(word(&map, at + 6)?);
        let end = refs + number * 12;
        if refs < previous_end || end > names
        {
            return Err(BAD_ARGUMENT);
        }
        previous_end = end;
        for record in map[refs..end].as_chunks::<12>().0
        {
            let name = word(record, 2)?;
            if name != u16::MAX
            {
                let start = names + usize::from(name);
                let len = usize::from(*map.get(start).ok_or(BAD_ARGUMENT)?);
                map.get(start + 1..start + 1 + len).ok_or(BAD_ARGUMENT)?;
            }
            let offset = u64::from(u32::from_be_bytes([0, record[5], record[6], record[7]]));
            bounded(offset, 4, data_len)?;
            let key = ResourceKey { kind,
                                    id: word(record, 0)? as i16 };
            if select(key)
            {
                if !seen.insert((key.kind, key.id))
                {
                    return Err(BAD_ARGUMENT);
                }
                selected.push((key, offset, record[4]));
            }
        }
    }
    let mut output = Vec::new();
    let mut remaining = limit;
    for (key, offset, attributes) in selected
    {
        let mut size = [0; 4];
        read(data + offset, &mut size)?;
        let size = u64::from(u32::from_be_bytes(size));
        bounded(offset + 4, size, data_len)?;
        remaining = remaining.checked_sub(size as usize).ok_or(BAD_ARGUMENT)?;
        let mut value = vec![0; size as usize];
        read(data + offset + 4, &mut value)?;
        output.push(Resource { kind: key.kind,
                               id: key.id,
                               attributes,
                               name: None,
                               data: value });
    }
    Ok(output)
}

fn decode(data: &[u8]) -> Result<VersionInfo, Status>
{
    let mut result = VersionInfo::default();
    result.number.copy_from_slice(data.get(..4).ok_or(BAD_ARGUMENT)?);
    result.country = word(data, 4)?;
    let mut at = 6;
    for (output, length) in
        [(&mut result.short, &mut result.short_length), (&mut result.long, &mut result.long_length)]
    {
        let size = usize::from(*data.get(at).ok_or(BAD_ARGUMENT)?);
        let text =
            mac_formats::text::decode(data.get(at + 1..at + 1 + size).ok_or(BAD_ARGUMENT)?);
        output[..text.len()].copy_from_slice(text.as_bytes());
        *length = text.len() as u32;
        at += 1 + size;
    }
    Ok(result)
}

/// # Safety
/// The request must be live and exclusively borrowed for its declared size.
pub(crate) unsafe fn dispatch(files: &mut crate::toolbox::files::Manager, pointer: *mut (),
                              size: usize)
                              -> i64
{
    if pointer.is_null()
       || size != size_of::<FileVersionCall>()
       || !(pointer as usize).is_multiple_of(align_of::<FileVersionCall>())
    {
        return BAD_ARGUMENT as i64;
    }
    let call = unsafe { &mut *pointer.cast::<FileVersionCall>() };
    if call.operation != FILE_VERSION || call.reserved != 0
    {
        return BAD_ARGUMENT as i64;
    }
    let result = files.resource_value(call.volume,
                                      call.cnid,
                                      ResourceKey { kind: *b"vers",
                                                    id:   call.id })
                      .and_then(|bytes| decode(&bytes));
    match result
    {
        Ok(info) =>
        {
            call.info = info;
            OK as i64
        }
        Err(status) => status as i64
    }
}
