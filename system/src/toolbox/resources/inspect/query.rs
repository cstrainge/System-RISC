//! Storage-independent resource queries. Readers satisfy a bounded length/data
//! request and replay the same query; no caller pointer or File Manager guard
//! belongs in this planner. Map validation and length checks live in Index.
#![forbid(unsafe_code)]
use mac_abi::{resources::*, *};
use mac_formats::resource::index::Index;

pub(crate) enum Need
{
    Done,
    Length
    {
        number: usize,
        offset: u64
    },
    Data
    {
        offset: u64,
        length: usize
    }
}
pub(crate) fn prepare(index: &Index, call: &mut FileResourceCall, name: &str, capacity: usize)
                      -> Result<Need, Status>
{
    match call.operation
    {
        FILE_FORMAT =>
        {
            call.index = match index.encoding()
            {
                mac_formats::resource::Encoding::Classic => 0,
                mac_formats::resource::Encoding::Native => 1
            };
            return Ok(Need::Done);
        }
        COUNT_TYPES =>
        {
            call.index = index.type_count() as u32;
            return Ok(Need::Done);
        }
        TYPE_INDEX =>
        {
            call.key.kind = index.type_at(call.index as usize).ok_or(NOT_FOUND)?;
            return Ok(Need::Done);
        }
        COUNT =>
        {
            call.index = index.count(call.key.kind) as u32;
            return Ok(Need::Done);
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
    let Some(length) = entry.length
    else
    {
        return Ok(Need::Length { number: entry.number,
                                 offset: entry.offset });
    };
    call.info = ResourceInfo { key: ResourceKey { kind: entry.kind,
                                                  id:   entry.id },
                               attributes: u16::from(entry.attributes),
                               length: length as usize,
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
        let size = (length as usize).checked_sub(call.offset).ok_or(BAD_ARGUMENT)?.min(capacity);
        if size != 0
        {
            return Ok(Need::Data { offset: entry.offset + 4 + call.offset as u64,
                                   length: size });
        }
    }
    Ok(Need::Done)
}
