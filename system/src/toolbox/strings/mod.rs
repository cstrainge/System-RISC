//! Legacy text conversion belongs to the resident trap boundary, never the SDK.
//! System 7 MacRoman (including the pre-8.5 currency sign at byte DB).
#![cfg_attr(not(any(target_arch = "riscv64", feature = "test-support")), allow(dead_code))]
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
    let text =
        core::str::from_utf8(bytes.get(..*length as usize).ok_or(BAD_ARGUMENT)?).map_err(|_| {
                                                                                    BAD_ARGUMENT
                                                                                })?;
    let encoded = encode(text)?;
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
            LOG =>
            {
                let original = get!(LogCall);
                let text = unsafe { input(original.bytes, original.length) }?;
                let mut call = LogCall { bytes:  text.as_ptr(),
                                         length: text.len() };
                Ok(invoke!(call))
            }
            WINDOW =>
            {
                let original = get!(WindowCall);
                let mut call = *original;
                if call.operation == NEW_WINDOW
                {
                    inline_input(&mut call.request.title, &mut call.request.title_length)?;
                }
                let status = invoke!(call);
                if status == OK as i64
                {
                    if matches!(call.operation, NEW_WINDOW | GET_WINDOW)
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
                let status = invoke!(call);
                if status == OK as i64
                   && matches!(call.operation,
                               resources::GET
                               | resources::NAMED
                               | resources::INDEX
                               | resources::INFO
                               | resources::READ
                               | resources::LOAD
                               | resources::RELEASE)
                {
                    inline_output(&mut call.info.name, &mut call.info.name_length)?;
                }
                call.name = original.name;
                call.name_length = original.name_length;
                *original = call;
                Ok(status)
            }
            FILE =>
            {
                let original = get!(FileCall);
                let mut call = *original;
                let text = if call.operation == files::LOOKUP
                {
                    unsafe { input(call.name, call.name_length) }?
                }
                else
                {
                    String::new()
                };
                call.name = text.as_ptr();
                call.name_length = text.len();
                let mut path =
                    if call.operation == files::PATH { vec![0; 4096] } else { Vec::new() };
                if call.operation == files::PATH
                {
                    call.output = path.as_mut_ptr();
                    call.capacity = path.len();
                }
                let status = invoke!(call);
                if status == OK as i64
                {
                    match call.operation
                    {
                        files::VOLUME_INDEX | files::VOLUME_INFO =>
                        {
                            inline_output(&mut call.volume_info.name,
                                          &mut call.volume_info.name_length)?
                        }
                        files::CATALOG | files::DIRECTORY_ENTRY | files::LOOKUP | files::PARENT =>
                        {
                            inline_output(&mut call.entry.name, &mut call.entry.name_length)?
                        }
                        files::PATH =>
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
            CONTROL =>
            {
                let original = get!(mac_abi::controls::ControlCall);
                let mut call = *original;
                if matches!(call.operation,
                            mac_abi::controls::DRAW_STANDARD | mac_abi::controls::HIT_STANDARD)
                {
                    inline_input(&mut call.standard.title, &mut call.standard.title_length)?;
                }
                let status = invoke!(call);
                if status == OK as i64
                {
                    call.standard = original.standard;
                    *original = call;
                }
                Ok(status)
            }
            _ => Err(NOT_SUPPORTED)
        }
    })();
    result.unwrap_or_else(|error| error as i64)
}
