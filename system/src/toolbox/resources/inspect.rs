//! Read-only, file-backed resource inspection. The File Manager owns and validates
//! the existing generation-tagged fork handle, including its application owner.
//! This service issues no additional IDs and never changes the resource chain.
use alloc::{collections::BTreeSet, vec, vec::Vec};

use mac_abi::{files::{FileCall, SEEK, SEEK_START, TELL},
              resources::*,
              *};

use super::file::{bounded, long, word};

struct Entry
{
    key:        ResourceKey,
    attributes: u8,
    name:       Option<Vec<u8>>,
    position:   u64,
    data_end:   u64
}
fn read(files: &mut crate::toolbox::files::Manager, owner: u64, handle: u32, offset: u64,
        output: &mut [u8])
        -> Result<(), Status>
{
    let mut call = FileCall { operation: SEEK,
                              handle,
                              whence: SEEK_START,
                              offset: i64::try_from(offset).map_err(|_| BAD_ARGUMENT)?,
                              ..Default::default() };
    files.request_owned(owner, &mut call, "", &mut [])?;
    call.operation = mac_abi::files::READ;
    files.request_owned(owner, &mut call, "", output)?;
    if call.transferred != output.len()
    {
        return Err(IO_ERROR);
    }
    Ok(())
}
fn entries(files: &mut crate::toolbox::files::Manager, owner: u64, handle: u32)
           -> Result<Vec<Entry>, Status>
{
    let mut call = FileCall { operation: TELL,
                              handle,
                              ..Default::default() };
    files.request_owned(owner, &mut call, "", &mut [])?;
    if call.length == 0
    {
        return Ok(Vec::new());
    }
    let mut header = [0; 16];
    bounded(0, 16, call.length)?;
    read(files, owner, handle, 0, &mut header)?;
    let data = u64::from(long(&header, 0)?);
    let map_at = u64::from(long(&header, 4)?);
    let data_len = u64::from(long(&header, 8)?);
    let map_len = u64::from(long(&header, 12)?);
    bounded(data, data_len, call.length)?;
    bounded(map_at, map_len, call.length)?;
    if data < 16 || map_at < 16 || data < map_at + map_len && map_at < data + data_len
    {
        return Err(BAD_ARGUMENT);
    }
    if map_len > 256 * 1024
    {
        return Err(NO_MEMORY);
    }
    let mut map = vec![0; map_len as usize];
    read(files, owner, handle, map_at, &mut map)?;
    let types = usize::from(word(&map, 24)?);
    let names = usize::from(word(&map, 26)?);
    if types < 28 || names < 28 || names > map.len()
    {
        return Err(BAD_ARGUMENT);
    }
    let count = word(&map, types)?;
    let count = if count == u16::MAX { 0 } else { usize::from(count) + 1 };
    let mut previous = types + 2 + count * 8;
    if previous > names
    {
        return Err(BAD_ARGUMENT);
    }
    let mut result = Vec::new();
    let mut kinds = BTreeSet::new();
    let mut keys = BTreeSet::new();
    for index in 0..count
    {
        let at = types + 2 + index * 8;
        let kind: [u8; 4] =
            map.get(at..at + 4).ok_or(BAD_ARGUMENT)?.try_into().map_err(|_| BAD_ARGUMENT)?;
        if !kinds.insert(kind)
        {
            return Err(BAD_ARGUMENT);
        }
        let number = usize::from(word(&map, at + 4)?) + 1;
        let refs = types + usize::from(word(&map, at + 6)?);
        let end = refs + number * 12;
        if refs < previous || end > names
        {
            return Err(BAD_ARGUMENT);
        }
        previous = end;
        for record in map[refs..end].as_chunks::<12>().0
        {
            let key = ResourceKey { kind,
                                    id: word(record, 0)? as i16 };
            if !keys.insert((kind, key.id))
            {
                return Err(BAD_ARGUMENT);
            }
            let name = word(record, 2)?;
            let name = if name == u16::MAX
            {
                None
            }
            else
            {
                let start = names + usize::from(name);
                let length = usize::from(*map.get(start).ok_or(BAD_ARGUMENT)?);
                Some(map.get(start + 1..start + 1 + length).ok_or(BAD_ARGUMENT)?.to_vec())
            };
            let offset = u64::from(u32::from_be_bytes([0, record[5], record[6], record[7]]));
            bounded(offset, 4, data_len)?;
            result.try_reserve(1).map_err(|_| NO_MEMORY)?;
            result.push(Entry { key,
                                attributes: record[4],
                                name,
                                position: data + offset,
                                data_end: data + data_len });
        }
    }
    Ok(result)
}
fn request(files: &mut crate::toolbox::files::Manager, owner: u64, call: &mut FileResourceCall,
           name: &str, output: &mut [u8])
           -> Result<(), Status>
{
    let entries = entries(files, owner, call.handle)?;
    match call.operation
    {
        COUNT_TYPES | TYPE_INDEX =>
        {
            let types: BTreeSet<_> = entries.iter().map(|e| e.key.kind).collect();
            if call.operation == COUNT_TYPES
            {
                call.index = types.len() as u32;
            }
            else
            {
                call.key.kind = *types.iter().nth(call.index as usize).ok_or(NOT_FOUND)?;
            }
            return Ok(());
        }
        COUNT =>
        {
            call.index = entries.iter().filter(|e| e.key.kind == call.key.kind).count() as u32;
            return Ok(());
        }
        GET | INFO | INDEX | NAMED | READ => (),
        _ => return Err(NOT_SUPPORTED)
    }
    let entry =
        match call.operation
        {
            INDEX =>
            {
                entries.iter().filter(|e| e.key.kind == call.key.kind).nth(call.index as usize)
            }
            NAMED =>
            {
                entries.iter().find(|e| {
                                  e.key.kind == call.key.kind
                                  && e.name
                                      .as_ref()
                                      .is_some_and(|n| mac_formats::text::decode(n) == name)
                              })
            }
            _ => entries.iter().find(|e| e.key == call.key)
        }.ok_or(NOT_FOUND)?;
    let mut length = [0; 4];
    read(files, owner, call.handle, entry.position, &mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    bounded(entry.position + 4, length as u64, entry.data_end)?;
    call.info = ResourceInfo { key: entry.key,
                               attributes: u16::from(entry.attributes),
                               length,
                               ..Default::default() };
    call.has_name = u32::from(entry.name.is_some());
    if let Some(name) = &entry.name
    {
        let name = mac_formats::text::decode(name);
        if name.len() > call.info.name.len()
        {
            return Err(BAD_ARGUMENT);
        }
        call.info.name[..name.len()].copy_from_slice(name.as_bytes());
        call.info.name_length = name.len() as u32;
    }
    call.key = entry.key;
    call.transferred = 0;
    if call.operation == READ
    {
        let available = length.checked_sub(call.offset).ok_or(BAD_ARGUMENT)?;
        let size = available.min(output.len());
        read(files,
             owner,
             call.handle,
             entry.position + 4 + call.offset as u64,
             &mut output[..size])?;
        call.transferred = size;
    }
    Ok(())
}
/// # Safety
/// Request and its declared borrowed buffers must be initialized and exclusive.
pub(crate) unsafe fn dispatch(files: &mut crate::toolbox::files::Manager, owner: u64,
                              pointer: *mut (), size: usize)
                              -> i64
{
    if pointer.is_null()
       || size != size_of::<FileResourceCall>()
       || !(pointer as usize).is_multiple_of(align_of::<FileResourceCall>())
    {
        return BAD_ARGUMENT as i64;
    }
    let call = unsafe { &mut *pointer.cast::<FileResourceCall>() };
    if call.reserved != [0; 2]
       || call.name_length > 768
       || call.capacity > 1024 * 1024
       || call.name_length != 0 && call.name.is_null()
       || call.capacity != 0 && call.output.is_null()
    {
        return BAD_ARGUMENT as i64;
    }
    let name = if call.name_length == 0
    {
        ""
    }
    else
    {
        match core::str::from_utf8(unsafe {
                  core::slice::from_raw_parts(call.name, call.name_length)
              })
        {
            Ok(name) => name,
            Err(_) => return BAD_ARGUMENT as i64
        }
    };
    let output = if call.capacity == 0
    {
        &mut [][..]
    }
    else
    {
        unsafe { core::slice::from_raw_parts_mut(call.output, call.capacity) }
    };
    request(files, owner, call, name, output).err().unwrap_or(OK) as i64
}
