//! Legacy text conversion belongs to the resident trap boundary, never the SDK.
//! System 7 MacRoman (including the pre-8.5 currency sign at byte DB).
#![cfg_attr(not(any(target_arch = "riscv64", feature = "test-support")), allow(dead_code))]
mod scrap;
mod textedit;
use alloc::{string::String, vec, vec::Vec};

use mac_abi::{files::FileCall, quickdraw::*, resources::ResourceCall, traps::*, *};

pub(crate) fn decode(bytes: &[u8]) -> Result<String, Status>
{
    let (&length, payload) = bytes.split_first().ok_or(BAD_ARGUMENT)?;
    if bytes.len() > 256 || payload.len() != length as usize
    {
        return Err(BAD_ARGUMENT);
    }
    Ok(mac_formats::text::decode(payload))
}
pub(crate) fn encode(text: &str) -> Result<Vec<u8>, Status>
{
    mac_formats::text::pstring(text).map_err(|_| BAD_ARGUMENT)
}
fn inline_input(bytes: &mut [u8], length: &mut u32) -> Result<(), Status>
{
    let text = decode(bytes.get(..*length as usize).ok_or(BAD_ARGUMENT)?)?;
    if text.len() > bytes.len()
    {
        return Err(BAD_ARGUMENT);
    }
    bytes.fill(0);
    bytes[..text.len()].copy_from_slice(text.as_bytes());
    *length = text.len() as u32;
    Ok(())
}
fn inline_output(bytes: &mut [u8], length: &mut u32) -> Result<(), Status>
{
    inline_output_with_limit(bytes, length, 255)
}
/// Str31 is a legacy filesystem-name projection, never a catalog mutation.
fn inline_name_output(bytes: &mut [u8], length: &mut u32) -> Result<(), Status>
{
    inline_output_with_limit(bytes, length, 31)
}
fn inline_output_with_limit(bytes: &mut [u8], length: &mut u32, limit: usize)
                            -> Result<(), Status>
{
    let text =
        core::str::from_utf8(bytes.get(..*length as usize).ok_or(BAD_ARGUMENT)?).map_err(|_| {
                                                                                    BAD_ARGUMENT
                                                                                })?;
    // Validate the entire name before projecting it: an unrepresentable suffix
    // must still fail, even when it lies beyond the returned Str31 prefix.
    let mut encoded = encode(text)?;
    encoded.truncate(limit + 1);
    encoded[0] = (encoded.len() - 1) as u8;
    if encoded.len() > bytes.len()
    {
        return Err(BAD_ARGUMENT);
    }
    bytes.fill(0);
    bytes[..encoded.len()].copy_from_slice(&encoded);
    *length = encoded.len() as u32;
    Ok(())
}
unsafe fn input(pointer: *const u8, length: usize) -> Result<String, Status>
{
    if pointer.is_null() || length == 0 || length > 256
    {
        return Err(BAD_ARGUMENT);
    }
    decode(unsafe { core::slice::from_raw_parts(pointer, length) })
}
unsafe fn request<'a, T: Copy>(pointer: *mut (), size: usize) -> Result<&'a mut T, Status>
{
    if pointer.is_null()
       || size != size_of::<T>()
       || !(pointer as usize).is_multiple_of(align_of::<T>())
    {
        return Err(BAD_ARGUMENT);
    }
    Ok(unsafe { &mut *pointer.cast::<T>() })
}
/// # Safety
/// The request and nested buffers must be live and exclusively borrowed just as
/// for the native gate. Temporary UTF-8 borrows never escape into the caller.
pub(crate) unsafe fn dispatch(number: u32, pointer: *mut (), size: usize,
                              mut native: impl FnMut(u32, *mut (), usize) -> i64)
                              -> i64
{
    let Some(number) = native_string_trap(number)
    else
    {
        return NOT_SUPPORTED as i64;
    };
    let result = (|| -> Result<i64, Status> {
        macro_rules! invoke {
            ($call:expr) => {
                native(number, core::ptr::from_mut(&mut $call).cast(), size)
            };
        }
        macro_rules! get {
            ($ty:ty) => {
                unsafe { request::<$ty>(pointer, size) }?
            };
        }
        match number
        {
            PROCESS =>
            {
                let original = get!(mac_abi::processes::ProcessCall);
                let mut call = *original;
                let status = invoke!(call);
                if status == OK as i64
                   && matches!(call.operation,
                               mac_abi::processes::CURRENT
                               | mac_abi::processes::INDEX
                               | mac_abi::processes::LAUNCH)
                {
                    inline_output(&mut call.info.name, &mut call.info.name_length)?;
                }
                *original = call;
                Ok(status)
            }

            SCRAP =>
            unsafe { scrap::dispatch(pointer, size, &mut native) },
            TEXTEDIT =>
            unsafe { textedit::dispatch(pointer, size, &mut native) },
            LOG =>
            {
                let original = get!(LogCall);
                let text = unsafe { input(original.bytes, original.length) }?;
                let mut call = LogCall { bytes:  text.as_ptr(),
                                         length: text.len() };
                Ok(invoke!(call))
            }
            STANDARD_FILE =>
            {
                let original = get!(mac_abi::standard_file::StandardFileCall);
                let mut call = *original;
                if call.operation == mac_abi::standard_file::BEGIN
                {
                    if call.prompt_length != 0
                    {
                        inline_input(&mut call.prompt, &mut call.prompt_length)?;
                    }
                    if call.name_length != 0
                    {
                        inline_input(&mut call.name, &mut call.name_length)?;
                    }
                }
                let status = invoke!(call);
                if status == OK as i64
                {
                    if call.operation != mac_abi::standard_file::DISPOSE
                       && call.state == mac_abi::standard_file::ACCEPTED
                    {
                        inline_name_output(&mut call.entry.name, &mut call.entry.name_length)?;
                    }
                    *original = call;
                }
                Ok(status)
            }
            DIALOG =>
            {
                let original = get!(mac_abi::dialogs::DialogCall);
                let mut call = *original;
                if matches!(call.operation,
                            mac_abi::dialogs::OPEN_ALERT | mac_abi::dialogs::VALIDATE_ALERT)
                {
                    inline_input(&mut call.text, &mut call.text_length)?;
                }
                let status = invoke!(call);
                if status == OK as i64
                {
                    original.window = call.window;
                    original.item = call.item;
                }
                Ok(status)
            }
            WINDOW =>
            {
                let original = get!(WindowCall);
                let mut call = *original;
                if matches!(call.operation, NEW_WINDOW | SET_WINDOW_TITLE)
                {
                    inline_input(&mut call.request.title, &mut call.request.title_length)?;
                }
                let status = invoke!(call);
                if status == OK as i64
                {
                    if matches!(call.operation, NEW_WINDOW | GET_WINDOW | SET_WINDOW_TITLE)
                    {
                        inline_output(&mut call.request.title, &mut call.request.title_length)?;
                    }
                    *original = call;
                }
                Ok(status)
            }
            DRAW =>
            {
                let original = get!(DrawCall);
                let mut call = *original;
                if matches!(call.command.kind, DRAW_TEXT | DRAW_BUTTON)
                {
                    inline_input(&mut call.command.text, &mut call.command.text_length)?;
                }
                Ok(invoke!(call))
            }
            SURFACE =>
            {
                let original = get!(SurfaceCall);
                let mut call = *original;
                let text = if matches!(call.operation, 4 | 6)
                {
                    unsafe { input(call.text, call.text_length) }?
                }
                else
                {
                    String::new()
                };
                call.text = text.as_ptr();
                call.text_length = text.len();
                let status = invoke!(call);
                call.text = original.text;
                call.text_length = original.text_length;
                *original = call;
                Ok(status)
            }
            FONT =>
            {
                let original = get!(FontCall);
                let mut call = *original;
                let text = if matches!(call.operation, FONT_ID | FONT_WIDTH | FONT_DRAW)
                {
                    unsafe { input(call.text, call.text_length) }?
                }
                else
                {
                    String::new()
                };
                call.text = text.as_ptr();
                call.text_length = text.len();
                let status = invoke!(call);
                if status == OK as i64 && matches!(call.operation, FONT_NAME | FONT_AT)
                {
                    inline_output(&mut call.name, &mut call.name_length)?;
                }
                call.text = original.text;
                call.text_length = original.text_length;
                *original = call;
                Ok(status)
            }
            RESOURCE =>
            {
                if size == size_of::<resources::FileResourceCall>()
                {
                    let original = get!(resources::FileResourceCall);
                    let mut call = *original;
                    let name = if call.operation == resources::NAMED
                    {
                        unsafe { input(call.name, call.name_length) }?
                    }
                    else
                    {
                        String::new()
                    };
                    call.name = name.as_ptr();
                    call.name_length = name.len();
                    let status = invoke!(call);
                    if status == OK as i64
                    {
                        if matches!(call.operation,
                                    resources::GET
                                    | resources::INDEX
                                    | resources::NAMED
                                    | resources::INFO
                                    | resources::READ)
                        {
                            inline_output(&mut call.info.name, &mut call.info.name_length)?;
                        }
                        call.name = original.name;
                        call.name_length = original.name_length;
                        *original = call;
                    }
                    return Ok(status);
                }
                if size == size_of::<resources::FileVersionCall>()
                {
                    let original = get!(resources::FileVersionCall);
                    let mut call = *original;
                    let status = invoke!(call);
                    if status == OK as i64
                    {
                        inline_output(&mut call.info.short, &mut call.info.short_length)?;
                        inline_output(&mut call.info.long, &mut call.info.long_length)?;
                        *original = call;
                    }
                    return Ok(status);
                }
                let original = get!(ResourceCall);
                let mut call = *original;
                let text = if call.operation == resources::NAMED
                {
                    unsafe { input(call.name, call.name_length) }?
                }
                else
                {
                    String::new()
                };
                call.name = text.as_ptr();
                call.name_length = text.len();
                let mut value = [0u8; INLINE_STRING_BYTES];
                if call.operation == resources::STRING
                {
                    call.output = value.as_mut_ptr();
                    call.capacity = value.len();
                }
                let status = invoke!(call);
                if status == OK as i64 && call.operation == resources::STRING
                {
                    let value = core::str::from_utf8(value.get(..call.transferred)
                                                          .ok_or(BAD_ARGUMENT)?).map_err(|_| {
                                                                                    BAD_ARGUMENT
                                                                                })?;
                    let encoded = encode(value)?;
                    if original.capacity < encoded.len()
                    {
                        return Ok(NO_MEMORY as i64);
                    }
                    if original.output.is_null()
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    // SAFETY: legacy output borrow is caller-owned for its declared capacity.
                    unsafe {
                        core::ptr::copy_nonoverlapping(encoded.as_ptr(),
                                                       original.output,
                                                       encoded.len());
                    }
                    call.transferred = encoded.len();
                    call.output = original.output;
                    call.capacity = original.capacity;
                }
                if status == OK as i64
                   && matches!(call.operation,
                               resources::GET
                               | resources::NAMED
                               | resources::INDEX
                               | resources::INFO
                               | resources::READ
                               | resources::LOAD
                               | resources::RELEASE
                               | resources::STRING)
                {
                    inline_output(&mut call.info.name, &mut call.info.name_length)?;
                }
                call.name = original.name;
                call.name_length = original.name_length;
                call.output = original.output;
                call.capacity = original.capacity;
                *original = call;
                Ok(status)
            }
            FILE =>
            {
                if size == size_of::<files::DataWriteCall>()
                {
                    let original = get!(files::DataWriteCall);
                    let mut call = *original;
                    let name = if call.operation == files::BEGIN_DATA_WRITE
                    {
                        unsafe { input(call.name, call.name_length) }?
                    }
                    else
                    {
                        String::new()
                    };
                    call.name = name.as_ptr();
                    call.name_length = name.len();
                    let status = invoke!(call);
                    if status == OK as i64
                    {
                        if call.operation == files::COMMIT_DATA_WRITE
                        {
                            inline_name_output(&mut call.entry.name, &mut call.entry.name_length)?;
                        }
                        call.name = original.name;
                        call.name_length = original.name_length;
                        *original = call;
                    }
                    return Ok(status);
                }
                if size == size_of::<files::FileScanCall>()
                {
                    let original = get!(files::FileScanCall);
                    let mut call = *original;
                    let status = invoke!(call);
                    if status == OK as i64
                    {
                        if call.step == files::SCAN_ENTRY
                        {
                            inline_name_output(&mut call.entry.name, &mut call.entry.name_length)?;
                        }
                        *original = call;
                    }
                    return Ok(status);
                }
                let original = get!(FileCall);
                let mut call = *original;
                let text = if matches!(call.operation,
                                       files::LOOKUP
                                       | files::NEW_FOLDER
                                       | files::RENAME
                                       | files::SET_COMMENT)
                {
                    unsafe { input(call.name, call.name_length) }?
                }
                else
                {
                    String::new()
                };
                call.name = text.as_ptr();
                call.name_length = text.len();
                let mut path = if matches!(call.operation, files::PATH | files::COMMENT)
                {
                    vec![0; 4096]
                }
                else
                {
                    Vec::new()
                };
                if matches!(call.operation, files::PATH | files::COMMENT)
                {
                    call.output = path.as_mut_ptr();
                    call.capacity = path.len();
                }
                let status = invoke!(call);
                if status == OK as i64
                {
                    match call.operation
                    {
                        files::VOLUME_INDEX
                        | files::VOLUME_INFO
                        | files::MOUNT_HOST_EXPORT
                        | files::MOUNT_FILE =>
                        {
                            inline_name_output(&mut call.volume_info.name,
                                               &mut call.volume_info.name_length)?
                        }
                        files::CATALOG
                        | files::DIRECTORY_ENTRY
                        | files::LOOKUP
                        | files::PARENT
                        | files::NEW_FOLDER
                        | files::TRASH_DIRECTORY
                        | files::MOVE_TO_TRASH
                        | files::PUT_AWAY
                        | files::RENAME
                        | files::SET_LOCKED
                        | files::SET_STATIONERY =>
                        {
                            inline_name_output(&mut call.entry.name, &mut call.entry.name_length)?
                        }
                        files::PATH | files::COMMENT =>
                        {
                            let text =
                                core::str::from_utf8(path.get(..call.transferred)
                                                         .ok_or(BAD_ARGUMENT)?).map_err(|_| {
                                                                                   BAD_ARGUMENT
                                                                               })?;
                            let encoded = encode(text)?;
                            call.length = encoded.len() as u64;
                            call.transferred = 0;
                            if original.capacity < encoded.len()
                            {
                                original.length = call.length;
                                return Ok(NO_MEMORY as i64);
                            }
                            if original.output.is_null()
                            {
                                return Err(BAD_ARGUMENT);
                            }
                            unsafe {
                                core::ptr::copy_nonoverlapping(encoded.as_ptr(),
                                                               original.output,
                                                               encoded.len());
                            }
                            call.transferred = encoded.len();
                        }
                        _ =>
                        {}
                    }
                }
                call.name = original.name;
                call.name_length = original.name_length;
                call.output = original.output;
                call.capacity = original.capacity;
                *original = call;
                Ok(status)
            }
            SCROLL_BOX =>
            {
                let original = get!(mac_abi::scrollbox::Call);
                let mut call = *original;
                if call.operation == mac_abi::scrollbox::SET_ROW
                {
                    inline_input(&mut call.text, &mut call.text_length)?;
                }
                let status = invoke!(call);
                call.text = original.text;
                call.text_length = original.text_length;
                *original = call;
                Ok(status)
            }
            CONTROL =>
            {
                let original = get!(mac_abi::controls::ControlCall);
                let mut call = *original;
                if matches!(call.operation,
                            mac_abi::controls::DRAW_STANDARD
                            | mac_abi::controls::HIT_STANDARD
                            | mac_abi::controls::CREATE_STANDARD)
                   || (call.operation == mac_abi::controls::CONFIGURE_OBJECT
                       && !call.standard.bounds.is_empty())
                {
                    inline_input(&mut call.standard.title, &mut call.standard.title_length)?;
                }
                let status = invoke!(call);
                if status == OK as i64
                {
                    if call.operation >= mac_abi::controls::CREATE_STANDARD
                    {
                        if !call.standard.bounds.is_empty()
                        {
                            inline_output(&mut call.standard.title,
                                          &mut call.standard.title_length)?;
                        }
                    }
                    else
                    {
                        call.standard = original.standard;
                    }
                    *original = call;
                }
                Ok(status)
            }
            _ => Err(NOT_SUPPORTED)
        }
    })();
    result.unwrap_or_else(|error| error as i64)
}
