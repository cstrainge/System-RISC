//! Arguments copied from the native startup snapshot into process-local storage.
pub use super::common::Args;
use crate::{ffi::{CStr, OsString},
            sync::OnceLock};

static ARGUMENTS: OnceLock<Vec<OsString>> = OnceLock::new();

/// Called once by platform startup with its checked, null-terminated vector.
pub unsafe fn init(argc: isize, argv: *const *const u8)
{
    if argc == 0
    {
        return;
    }
    let pointers = unsafe { crate::slice::from_raw_parts(argv, argc as usize) };
    let values =
        pointers.iter()
                .map(|&pointer| {
                    let argument = unsafe { CStr::from_ptr(pointer.cast()) };
                    // Startup validates each argument's UTF-8 before calling main.
                    OsString::from(unsafe { crate::str::from_utf8_unchecked(argument.to_bytes()) })
                })
                .collect();
    let _ = ARGUMENTS.set(values);
}

pub fn args() -> Args
{
    Args::new(ARGUMENTS.get().cloned().unwrap_or_default())
}
