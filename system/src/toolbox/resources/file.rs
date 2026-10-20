//! Bounded file-backed metadata without changing the resident resource chain.
use alloc::vec::Vec;

use mac_abi::{resources::*, *};
use mac_formats::resource::Resource;

pub(super) fn word(bytes: &[u8], at: usize) -> Result<u16, Status>
{
    Ok(u16::from_be_bytes(bytes.get(at..at + 2)
                               .ok_or(BAD_ARGUMENT)?
                               .try_into()
                               .map_err(|_| BAD_ARGUMENT)?))
}
pub(super) fn long(bytes: &[u8], at: usize) -> Result<u32, Status>
{
    Ok(u32::from_be_bytes(bytes.get(at..at + 4)
                               .ok_or(BAD_ARGUMENT)?
                               .try_into()
                               .map_err(|_| BAD_ARGUMENT)?))
}
pub(super) fn bounded(start: u64, size: u64, total: u64) -> Result<(), Status>
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
    let mut io_error = None;
    mac_formats::resource::stream::read_selected(length,
        mac_formats::resource::stream::Budget {payload_bytes:limit,..Default::default()},
        |kind,id| select(ResourceKey {kind,id}),
        |offset,output| read(offset,output).map_err(|status| {
            io_error=Some(status);
            mac_formats::Error("resource storage read failed")
        }))
        .map_err(|error| io_error.unwrap_or(if error.0.contains("budget") || error.0.contains("allocation") {NO_MEMORY} else {BAD_ARGUMENT}))
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
