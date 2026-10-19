//! Pascal/MacRoman text convenience trap; offsets still use the native UTF-8
//! profile. This is not the original TEHandle/MacRoman-offset A-line adapter.
use mac_abi::{textedit::*, traps::TEXTEDIT, *};

/// # Safety
/// The request and nested buffers satisfy the native service borrowing contract.
/// Converted temporary UTF-8 data is borrowed only until the native call returns.
pub(super) unsafe fn dispatch(pointer: *mut (), size: usize,
                              native: &mut impl FnMut(u32, *mut (), usize) -> i64)
                              -> Result<i64, Status>
{
    let original = unsafe { super::request::<TextEditCall>(pointer, size) }?;
    let mut call = *original;
    let result = match call.operation
    {
        SET_TEXT | INSERT =>
        {
            let text = unsafe { super::input(call.input, call.input_length) }?;
            call.input = text.as_ptr();
            call.input_length = text.len();
            native(TEXTEDIT, core::ptr::from_mut(&mut call).cast(), size)
        }
        TEXT | SELECTION_TEXT =>
        {
            // A representable255-character MacRoman string needs at most765
            // UTF-8 bytes. Longer text cannot fit any Pascal string.
            let mut bytes = [0u8; INLINE_STRING_BYTES];
            call.output = bytes.as_mut_ptr();
            call.capacity = bytes.len();
            let status = native(TEXTEDIT, core::ptr::from_mut(&mut call).cast(), size);
            if status != OK as i64
            {
                return Ok(if status == NO_MEMORY as i64 { BAD_ARGUMENT as i64 } else { status });
            }
            let text = core::str::from_utf8(bytes.get(..call.transferred).ok_or(BAD_ARGUMENT)?)
                .map_err(|_| BAD_ARGUMENT)?;
            let encoded = super::encode(text)?;
            if original.capacity < encoded.len()
            {
                return Ok(NO_MEMORY as i64);
            }
            if original.output.is_null()
            {
                return Err(BAD_ARGUMENT);
            }
            // SAFETY: the caller owns the declared output borrow and the encoded
            // Pascal string fits; no caller buffer is touched before conversion.
            unsafe {
                core::ptr::copy_nonoverlapping(encoded.as_ptr(), original.output, encoded.len());
            }
            call.transferred = encoded.len();
            status
        }
        _ => return Err(BAD_ARGUMENT)
    };
    if result == OK as i64
    {
        call.input = original.input;
        call.input_length = original.input_length;
        call.output = original.output;
        call.capacity = original.capacity;
        *original = call;
    }
    Ok(result)
}
