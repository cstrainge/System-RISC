//! Read-only, file-backed resource inspection. The File Manager owns and validates
//! the existing generation-tagged fork handle, including its application owner.
//! This service issues no additional IDs and never changes the resource chain.
use alloc::vec::Vec;

use mac_abi::{files::{FileCall, SEEK, SEEK_START, TELL},
              resources::*,
              *};
use mac_formats::resource::{index::{Index, MapLayout},
                            stream::Budget};

fn format_status(error: mac_formats::Error) -> Status
{
    if error.0.contains("budget") || error.0.contains("allocation")
    {
        NO_MEMORY
    }
    else
    {
        BAD_ARGUMENT
    }
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
           -> Result<Index, Status>
{
    let mut call = FileCall { operation: TELL,
                              handle,
                              ..Default::default() };
    files.request_owned(owner, &mut call, "", &mut [])?;
    if call.length == 0
    {
        return Ok(Index::empty());
    }
    let mut header = [0; 16];
    if call.length < header.len() as u64
    {
        return Err(BAD_ARGUMENT);
    }
    read(files, owner, handle, 0, &mut header)?;
    let layout = MapLayout::decode(&header, call.length, Budget::default()).map_err(format_status)?;
    let mut map = Vec::new();
    map.try_reserve_exact(layout.map_length()).map_err(|_| NO_MEMORY)?;
    map.resize(layout.map_length(), 0);
    read(files, owner, handle, layout.map_offset(), &mut map)?;
    Index::decode(layout, map).map_err(format_status)
}
fn query(files: &mut crate::toolbox::files::Manager, owner: u64, call: &mut FileResourceCall,
         name: &str, output: &mut [u8], index: &mut Index)
         -> Result<(), Status>
{
    match call.operation
    {
        COUNT_TYPES =>
        {
            call.index = index.type_count() as u32;
            return Ok(());
        }
        TYPE_INDEX =>
        {
            call.key.kind = index.type_at(call.index as usize).ok_or(NOT_FOUND)?;
            return Ok(());
        }
        COUNT =>
        {
            call.index = index.count(call.key.kind) as u32;
            return Ok(());
        }
        GET | INFO | INDEX | NAMED | READ => (),
        _ => return Err(NOT_SUPPORTED)
    }
    let entry = match call.operation
                {
                    INDEX => index.at(call.key.kind, call.index as usize),
                    NAMED => index.named(call.key.kind, name),
                    _ => index.get(call.key.kind, call.key.id)
                }.ok_or(NOT_FOUND)?;
    let (number, position, cached) = (entry.number, entry.offset, entry.length);
    let length = if let Some(length) = cached
    {
        length
    }
    else
    {
        let mut bytes = [0; 4];
        read(files, owner, call.handle, position, &mut bytes)?;
        let length = u32::from_be_bytes(bytes);
        index.record_length(number, length).map_err(format_status)?;
        length
    } as usize;
    let entry = index.entry(number).ok_or(BAD_ARGUMENT)?;
    call.info = ResourceInfo { key: ResourceKey { kind: entry.kind,
                                                  id:   entry.id },
                               attributes: u16::from(entry.attributes),
                               length,
                               ..Default::default() };
    call.has_name = u32::from(entry.name.is_some());
    if let Some(name) = entry.name
    {
        let mut used = 0;
        for character in name.iter().map(|&byte| mac_formats::text::character(byte))
        {
            let mut bytes = [0; 4];
            let bytes = character.encode_utf8(&mut bytes).as_bytes();
            let end = used + bytes.len();
            call.info.name.get_mut(used..end).ok_or(BAD_ARGUMENT)?.copy_from_slice(bytes);
            used = end;
        }
        call.info.name_length = used as u32;
    }
    call.key = call.info.key;
    call.transferred = 0;
    if call.operation == READ
    {
        let size = length.checked_sub(call.offset).ok_or(BAD_ARGUMENT)?.min(output.len());
        if size != 0
        {
            read(files,
                 owner,
                 call.handle,
                 position + 4 + call.offset as u64,
                 &mut output[..size])?;
        }
        call.transferred = size;
    }
    Ok(())
}
fn request(files: &mut crate::toolbox::files::Manager, owner: u64, call: &mut FileResourceCall,
           name: &str, output: &mut [u8])
           -> Result<(), Status>
{
    // The cache belongs to this generation-checked, owner-checked open mark.
    // Failed queries retain it; closing/reusing the slot drops it with the fork.
    let cached = files.take_resource_index(owner, call.handle)?;
    if !matches!(call.operation,
                 COUNT_TYPES | TYPE_INDEX | COUNT | GET | INFO | INDEX | NAMED | READ)
    {
        if let Some(index) = cached
        {
            files.restore_resource_index(owner, call.handle, index)?;
        }
        return Err(NOT_SUPPORTED);
    }
    let mut index = match cached
    {
        Some(index) => index,
        None => entries(files, owner, call.handle)?
    };
    let result = query(files, owner, call, name, output, &mut index);
    files.restore_resource_index(owner, call.handle, index)?;
    result
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
