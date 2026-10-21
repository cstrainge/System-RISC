//! Typed service failures. Numeric status codes belong to the trap contract.
use mac_abi::{Status, *};

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    BadAbi,
    BadArgument,
    NotFound,
    NotSupported,
    NoMemory,
    TranslationRequired,
    ServiceBusy,
    WrongThread,
    Io,
    ReadOnly,
    AlreadyExists,
    AccessDenied,
    DiskFull,
    RollbackFailed,
    DirectoryNotEmpty,
    ScanStale,
    Cancelled,
    CorruptData,
    StorageUnavailable,
    EventNotEnabled,
    ScrapTypeUnavailable,
    MemoryFull,
    NilHandle,
    InvalidZone,
    HandleNotPurgeable,
    HandleLocked,
    /// An unrecognized service status, retained losslessly for diagnostics.
    Unknown(Status)
}
impl Error
{
    pub const fn from_status(status: Status) -> Self
    {
        match status
        {
            BAD_ABI => Self::BadAbi,
            BAD_ARGUMENT => Self::BadArgument,
            NOT_FOUND => Self::NotFound,
            NOT_SUPPORTED => Self::NotSupported,
            NO_MEMORY => Self::NoMemory,
            TRANSLATION_REQUIRED => Self::TranslationRequired,
            SERVICE_BUSY => Self::ServiceBusy,
            WRONG_THREAD => Self::WrongThread,
            IO_ERROR => Self::Io,
            READ_ONLY => Self::ReadOnly,
            ALREADY_EXISTS => Self::AlreadyExists,
            ACCESS_DENIED => Self::AccessDenied,
            DISK_FULL => Self::DiskFull,
            ROLLBACK_FAILED => Self::RollbackFailed,
            DIRECTORY_NOT_EMPTY => Self::DirectoryNotEmpty,
            SCAN_STALE => Self::ScanStale,
            OPERATION_CANCELLED => Self::Cancelled,
            CORRUPT_DATA => Self::CorruptData,
            STORAGE_UNAVAILABLE => Self::StorageUnavailable,
            events::EVENT_NOT_ENABLED => Self::EventNotEnabled,
            scrap::NO_TYPE_ERR => Self::ScrapTypeUnavailable,
            memory::MEM_FULL_ERR => Self::MemoryFull,
            memory::NIL_HANDLE_ERR => Self::NilHandle,
            memory::MEM_WZ_ERR => Self::InvalidZone,
            memory::MEM_PUR_ERR => Self::HandleNotPurgeable,
            memory::MEM_LOCKED_ERR => Self::HandleLocked,
            other => Self::Unknown(other)
        }
    }

    pub const fn status(self) -> Status
    {
        match self
        {
            Self::BadAbi => BAD_ABI,
            Self::BadArgument => BAD_ARGUMENT,
            Self::NotFound => NOT_FOUND,
            Self::NotSupported => NOT_SUPPORTED,
            Self::NoMemory => NO_MEMORY,
            Self::TranslationRequired => TRANSLATION_REQUIRED,
            Self::ServiceBusy => SERVICE_BUSY,
            Self::WrongThread => WRONG_THREAD,
            Self::Io => IO_ERROR,
            Self::ReadOnly => READ_ONLY,
            Self::AlreadyExists => ALREADY_EXISTS,
            Self::AccessDenied => ACCESS_DENIED,
            Self::DiskFull => DISK_FULL,
            Self::RollbackFailed => ROLLBACK_FAILED,
            Self::DirectoryNotEmpty => DIRECTORY_NOT_EMPTY,
            Self::ScanStale => SCAN_STALE,
            Self::Cancelled => OPERATION_CANCELLED,
            Self::CorruptData => CORRUPT_DATA,
            Self::StorageUnavailable => STORAGE_UNAVAILABLE,
            Self::EventNotEnabled => events::EVENT_NOT_ENABLED,
            Self::ScrapTypeUnavailable => scrap::NO_TYPE_ERR,
            Self::MemoryFull => memory::MEM_FULL_ERR,
            Self::NilHandle => memory::NIL_HANDLE_ERR,
            Self::InvalidZone => memory::MEM_WZ_ERR,
            Self::HandleNotPurgeable => memory::MEM_PUR_ERR,
            Self::HandleLocked => memory::MEM_LOCKED_ERR,
            Self::Unknown(status) => status
        }
    }
}
impl core::fmt::Display for Error
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::BadAbi => f.write_str("incompatible System ABI"),
            Self::BadArgument => f.write_str("invalid argument"),
            Self::NotFound => f.write_str("not found"),
            Self::NotSupported => f.write_str("service not supported"),
            Self::NoMemory => f.write_str("out of memory"),
            Self::TranslationRequired => f.write_str("68k translation required"),
            Self::ServiceBusy => f.write_str("System service is busy"),
            Self::WrongThread => f.write_str("operation requires its owning OS thread"),
            Self::Io => f.write_str("disk I/O failed"),
            Self::ReadOnly => f.write_str("volume is read-only"),
            Self::AlreadyExists => f.write_str("an item with that name already exists"),
            Self::AccessDenied => f.write_str("item is protected or locked"),
            Self::DiskFull => f.write_str("metadata storage is full"),
            Self::RollbackFailed => f.write_str("disk rollback failed; remount required"),
            Self::DirectoryNotEmpty => f.write_str("directory is not empty"),
            Self::ScanStale => f.write_str("directory scan invalidated by a catalog change"),
            Self::Cancelled => f.write_str("operation cancelled"),
            Self::CorruptData => f.write_str("file or catalog data is damaged or malformed"),
            Self::StorageUnavailable => f.write_str("backing storage is unavailable"),
            Self::EventNotEnabled => f.write_str("event type is disabled by the OS event mask"),
            Self::ScrapTypeUnavailable => f.write_str("clipboard flavor is unavailable"),
            Self::MemoryFull => f.write_str("Memory Manager allocation failed"),
            Self::NilHandle => f.write_str("invalid or empty memory handle"),
            Self::InvalidZone => f.write_str("invalid memory zone"),
            Self::HandleNotPurgeable =>
            {
                f.write_str("locked handle cannot be emptied or reallocated")
            }
            Self::HandleLocked => f.write_str("memory handle is locked"),
            Self::Unknown(status) => write!(f, "unknown System service status {status}")
        }
    }
}
impl core::error::Error for Error
{
}
impl From<Status> for Error
{
    fn from(status: Status) -> Self
    {
        Self::from_status(status)
    }
}
