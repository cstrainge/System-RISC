//! Read-only, file-backed resource inspection. The File Manager owns and validates
//! the existing generation-tagged fork handle, including its application owner.
//! This service issues no additional IDs and never changes the resource chain.
use alloc::vec::Vec;

use mac_abi::{files::{FileCall, SEEK, SEEK_START, TELL},
              resources::*,
              *};
use mac_formats::resource::{index::{Index, MapLayout},
                            stream::Budget};

mod query;
pub(crate) use query::{Need, prepare};
pub(crate) fn format_status(error: mac_formats::Error) -> Status
{
    match error
    {
        mac_formats::Error::BudgetExceeded(_) | mac_formats::Error::AllocationFailed(_) =>
        {
            NO_MEMORY
        }
        mac_formats::Error::Missing(_) => NOT_FOUND,
        mac_formats::Error::TranslationRequired(_) => TRANSLATION_REQUIRED,
        mac_formats::Error::Malformed(_) => BAD_ARGUMENT
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
fn run_query(files: &mut crate::toolbox::files::Manager, owner: u64, call: &mut FileResourceCall,
             name: &str, output: &mut [u8], index: &mut Index)
             -> Result<(), Status>
{
    loop
    {
        match prepare(index, call, name, output.len())?
        {
            Need::Done => return Ok(()),
            Need::Length { number,
                           offset } =>
            {
                let mut bytes = [0; 4];
                read(files, owner, call.handle, offset, &mut bytes)?;
                index.record_length(number, u32::from_be_bytes(bytes)).map_err(format_status)?;
            }
            Need::Data { offset,
                         length } =>
            {
                read(files, owner, call.handle, offset, &mut output[..length])?;
                call.transferred = length;
                return Ok(());
            }
        }
    }
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
    let result = run_query(files, owner, call, name, output, &mut index);
    files.restore_resource_index(owner, call.handle, index)?;
    result
}
/// # Safety
/// Request and its declared borrowed buffers must be initialized and exclusive.
#[cfg(target_arch = "riscv64")]
pub(crate) unsafe fn dispatch_async(owner: u64, pointer: *mut (), size: usize) -> i64
{
    unsafe {
        checked(pointer, size, |call, name, output| {
            let routed = crate::kernel::with_files(|files| {
                match files.resource_provider(owner, call.handle)?
                {
                    None => request(files, owner, call, name, output).map(|()| true),
                    Some(_) => files.cached_resource_query(owner, call, name, output.len())
                }
            });
            match routed
            {
                Ok(Ok(true)) => OK,
                Ok(Ok(false)) => crate::toolbox::files::request_resource(owner, call, name, output),
                Ok(Err(error)) => error,
                Err(error) => error.status()
            }
        })
    }
}
/// # Safety
/// Request and its declared borrowed buffers must be initialized and exclusive.
#[cfg(not(target_arch = "riscv64"))]
pub(crate) unsafe fn dispatch(files: &mut crate::toolbox::files::Manager, owner: u64,
                              pointer: *mut (), size: usize)
                              -> i64
{
    unsafe {
        checked(pointer, size, |call, name, output| {
            request(files, owner, call, name, output).err().unwrap_or(OK)
        })
    }
}
/// # Safety
/// All request and nested buffers are live exclusive borrows for this call.
/// The callback must retain no caller borrow when it requests an I/O wait.
unsafe fn checked(pointer: *mut (), size: usize,
                  invoke: impl FnOnce(&mut FileResourceCall, &str, &mut [u8]) -> Status)
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
    invoke(call, name, output) as i64
}

#[cfg(test)]
mod typed_error_tests
{
    use super::*;
    #[test]
    fn status_uses_cause_even_when_the_message_uses_misleading_vocabulary()
    {
        use mac_formats::Error;
        for message in ["limit exceeded", "alloc failed", "absent entry",
                        "allocation budget missing translation"]
        {
            assert_eq!(format_status(Error::BudgetExceeded(message)), NO_MEMORY);
            assert_eq!(format_status(Error::AllocationFailed(message)), NO_MEMORY);
            assert_eq!(format_status(Error::Missing(message)), NOT_FOUND);
            assert_eq!(format_status(Error::TranslationRequired(message)), TRANSLATION_REQUIRED);
            assert_eq!(format_status(Error::Malformed(message)), BAD_ARGUMENT);
        }
    }
}
