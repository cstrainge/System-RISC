use mac_abi::*;

use crate::{fmt, io::ErrorKind};

pub fn errno() -> i32
{
    // Native calls return their status directly. There is no ambient errno.
    NOT_SUPPORTED
}
pub fn is_interrupted(_code: i32) -> bool
{
    // Explicit cancellation must not become an automatic std retry.
    false
}
pub fn decode_error_kind(code: i32) -> ErrorKind
{
    match code
    {
        BAD_ARGUMENT | INVALID_NAME => ErrorKind::InvalidInput,
        NOT_FOUND => ErrorKind::NotFound,
        NOT_SUPPORTED => ErrorKind::Unsupported,
        NO_MEMORY => ErrorKind::OutOfMemory,
        SERVICE_BUSY => ErrorKind::WouldBlock,
        READ_ONLY => ErrorKind::ReadOnlyFilesystem,
        ACCESS_DENIED | WRONG_THREAD => ErrorKind::PermissionDenied,
        ALREADY_EXISTS => ErrorKind::AlreadyExists,
        DISK_FULL => ErrorKind::StorageFull,
        DIRECTORY_NOT_EMPTY => ErrorKind::DirectoryNotEmpty,
        CORRUPT_DATA | BAD_ABI => ErrorKind::InvalidData,
        STORAGE_UNAVAILABLE => ErrorKind::NotConnected,
        _ => ErrorKind::Other
    }
}
pub fn format_error(code: i32, f: &mut fmt::Formatter<'_>) -> fmt::Result
{
    if code == OK
    {
        return f.write_str("operation successful");
    }
    fmt::Display::fmt(&mac_traps::Error::from_status(code), f)
}
