//! Native RV64 service ABI. This is separate from the future 32-bit 68k ABI.
#![no_std]
#![forbid(unsafe_code)]

pub const ABI_VERSION: u32 = 8;
pub type Status = i32;
pub const OK: Status = 0;
pub const BAD_ABI: Status = -1;
pub const BAD_ARGUMENT: Status = -2;
pub const NOT_FOUND: Status = -3;
pub const NOT_SUPPORTED: Status = -4;
pub const NO_MEMORY: Status = -5;
pub const TRANSLATION_REQUIRED: Status = -6;
/// Temporary contention; scheduler-aware blocking is a future service policy.
pub const SERVICE_BUSY: Status = -7;
/// Operation requires its owning OS thread/service context.
pub const WRONG_THREAD: Status = -8;
pub const IO_ERROR: Status = -9;
pub const READ_ONLY: Status = -10;
pub const ALREADY_EXISTS: Status = -11;
pub const ACCESS_DENIED: Status = -12;
pub const DISK_FULL: Status = -13;
pub const ROLLBACK_FAILED: Status = -14;
pub const DIRECTORY_NOT_EMPTY: Status = -15;
/// A catalog mutation invalidated an incremental directory scan; restart totals.
pub const SCAN_STALE: Status = -16;
/// Cancellation was acknowledged before the requested operation completed.
pub const OPERATION_CANCELLED: Status = -17;
/// Stored file/container metadata or verified payload is malformed.
pub const CORRUPT_DATA: Status = -18;
/// A mounted provider's backing device/export is no longer available.
pub const STORAGE_UNAVAILABLE: Status = -19;
pub const INVALID_NAME: Status = -20;

pub mod crashes;
pub mod dates;
pub mod geometry;
pub mod events;
pub mod drag;
pub mod scrap;
pub mod textedit;
pub mod menus;
pub mod memory;
pub mod images;
pub use events::*;
pub use geometry::{Point, Rect};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct ResourceKey
{
    pub kind: [u8; 4],
    pub id:   i16
}

impl ResourceKey
{
    pub const fn from_bytes(kind: [u8; 4], id: i16) -> Self
    {
        Self { kind,
               id }
    }
}

/// Firmware block devices remain available while System is running. Device
/// ordinals are stable; NOT_FOUND terminates enumeration. Reads transfer one
/// 512-byte sector into caller-owned writable storage.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DiskInfo
{
    pub sector_count: u64,
    pub read_only:    u32,
    pub icon_kind:    [u8; 4],
    pub icon_id:      i16,
    pub reserved:     u16
}
pub type DiskInfoFn =
    unsafe extern "C" fn(context: *mut (), index: u32, output: *mut DiskInfo) -> Status;
pub type ReadSectorFn =
    unsafe extern "C" fn(context: *mut (), index: u32, sector: u64, output: *mut u8) -> Status;

/// Transfer exactly one512-byte sector from exclusive caller-owned readable storage.
pub type WriteSectorFn =
    unsafe extern "C" fn(context: *mut (), index: u32, sector: u64, input: *const u8) -> Status;
/// Complete all preceding writes to durable storage, or report an I/O error.
pub type FlushDiskFn = unsafe extern "C" fn(context: *mut (), index: u32) -> Status;

/// The pointed-to bytes must remain readable for the duration of the call.
pub type LogFn = unsafe extern "C" fn(bytes: *const u8, length: usize) -> Status;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Header
{
    pub version: u32,
    pub size:    u32
}

impl Header
{
    pub const fn for_type<T>() -> Self
    {
        Self { version: ABI_VERSION,
               size:    core::mem::size_of::<T>() as u32 }
    }

    pub const fn supports<T>(&self) -> bool
    {
        self.version == ABI_VERSION && self.size as usize >= core::mem::size_of::<T>()
    }
}

pub const BOOT_GRAPHICAL: u32 = 1;
/// Supplied by the bootstrap; the loader owns the stack and zeroes native BSS.
#[repr(C)]
pub struct BootInfo
{
    pub header:                  Header,
    pub ram_base:                u64,
    pub ram_size:                u64,
    pub log:                     Option<LogFn>,
    pub storage_context:         *mut (),
    pub disk_info:               Option<DiskInfoFn>,
    pub read_sector:             Option<ReadSectorFn>,
    pub write_sector:            Option<WriteSectorFn>,
    pub flush_disk:              Option<FlushDiskFn>,
    pub startup_disk:            u32,
    pub flags:                   u32,
    pub startup_partition_start: u64
}

/// Resident in System memory for the entire application lifetime.
#[repr(C)]
pub struct SystemServices
{
    pub header:       Header,
    pub trap:         TrapGate,
    pub capabilities: u64
}
pub type TrapGate = unsafe extern "C" fn(number: u32, request: *mut (), size: usize) -> i64;
pub const CAP_GRAPHICS: u64 = 1;
pub const CAP_EVENT_WAIT: u64 = 2;
pub const CAP_MONOTONIC_CLOCK: u64 = 4;

pub const NEW_WINDOW: u32 = 0;
pub const CLOSE_WINDOW: u32 = 1;
pub const SELECT_WINDOW: u32 = 2;
pub const GET_WINDOW: u32 = 3;
pub const INVALIDATE_WINDOW: u32 = 4;
pub const BEGIN_UPDATE: u32 = 5;
pub const END_UPDATE: u32 = 6;
/// Deactivate every window while retaining their z order.
pub const SELECT_DESKTOP: u32 = 7;
/// Restrict automatic WDEF grow drawing to a window-local rectangle. A reserved
/// value of1 restores the default unrestricted drawing;0 applies bounds.
pub const SET_GROW_ICON_CLIP: u32 = 8;
/// Change a window's UTF8 title without disturbing its contents or position.
pub const SET_WINDOW_TITLE: u32 = 9;
/// Set reserved to0 for automatic close,1 for an application close request.
pub const SET_CLOSE_DEFERRED: u32 = 10;
/// OsEvent message prefix; low24 bits contain the WindowId.
pub const WINDOW_CLOSE_REQUEST: u32 = 0x03000000;
/// A classic 31-byte MacRoman catalog name may occupy up to93 UTF8 bytes.
pub const INLINE_STRING_BYTES: usize = 768;
pub const WINDOW_TITLE_BYTES: usize = INLINE_STRING_BYTES;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WindowRequest
{
    pub id:             u32,
    pub bounds:         Rect,
    pub title:          [u8; WINDOW_TITLE_BYTES],
    pub title_length:   u32,
    pub procedure:      i16,
    pub minimum_width:  i16,
    pub minimum_height: i16,
    pub reserved:       i16
}

impl WindowRequest
{
    /// Native UTF-8 title returned by the resident Window Manager.
    pub fn title(&self) -> &str
    {
        core::str::from_utf8(&self.title[..(self.title_length as usize).min(self.title.len())]).unwrap_or("")
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DrawCommand
{
    pub kind:        u32,
    pub bounds:      Rect,
    pub color:       u32,
    pub text_length: u32,
    pub text:        [u8; INLINE_STRING_BYTES]
}
pub const PAINT_RECT: u32 = 0;
pub const FRAME_RECT: u32 = 1;
pub const DRAW_TEXT: u32 = 2;
pub const DRAW_BUTTON: u32 = 3;

#[repr(C)]
pub struct Surface
{
    pub pixels: *mut u32,
    pub width:  u32,
    pub height: u32,
    pub origin: Point,
    pub clip:   Rect
}

#[repr(C)]
pub struct DefinitionCall
{
    pub header:    Header,
    pub kind:      u32,
    pub id:        i16,
    pub variation: i16,
    pub message:   i16,
    pub reserved:  u16,
    pub param:     i64,
    pub object:    *mut (),
    pub surface:   *mut Surface,
    pub gate:      TrapGate
}
pub type DefinitionEntry = unsafe extern "C" fn(*mut DefinitionCall) -> i64;

pub type SystemEntry = unsafe extern "C" fn(*const BootInfo) -> *const SystemServices;
pub type ApplicationEntry = unsafe extern "C" fn(*const SystemServices) -> Status;

#[cfg(test)]
mod tests
{
    use super::*;

    #[test]
    fn rv64_layout_and_version_contract()
    {
        assert_eq!(core::mem::size_of::<Header>(), 8);
        assert_eq!(core::mem::offset_of!(BootInfo, ram_base), 8);
        assert_eq!(core::mem::offset_of!(BootInfo, log), 24);
        assert_eq!(core::mem::size_of::<BootInfo>(), 88);
        assert_eq!(core::mem::offset_of!(BootInfo, storage_context), 32);
        assert_eq!(core::mem::offset_of!(BootInfo, startup_partition_start), 80);
        assert_eq!(core::mem::offset_of!(SystemServices, trap), 8);
        assert_eq!(core::mem::size_of::<SystemServices>(), 24);
        assert!(Header::for_type::<SystemServices>().supports::<SystemServices>());
        assert!(!Header { version: ABI_VERSION + 1,
                          size:    16 }.supports::<SystemServices>());
        assert!(!Header { version: 1,
                          size:    8 }.supports::<SystemServices>());
    }
}

pub mod traps;
pub mod quickdraw;
pub mod resources;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct WindowId(pub u32);
impl core::fmt::Display for WindowId
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        self.0.fmt(f)
    }
}


pub mod definitions;

pub mod files;

pub mod controls;

pub mod dialogs;

pub mod processes;

pub mod standard_file;

pub mod scrollbox;
