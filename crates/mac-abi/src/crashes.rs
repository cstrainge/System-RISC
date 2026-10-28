//! System-owned native panic reports. No classic Macintosh trap number is used.
use crate::WindowId;
pub const BEGIN: u32 = 0;
pub const APPEND: u32 = 1;
pub const SHOW: u32 = 2;
pub const INFO: u32 = 3;
pub const READ: u32 = 4;
pub const SCROLL: u32 = 5;
pub const DISMISS: u32 = 6;
/// Resident retention limit; excess diagnostics remain available in the serial log.
pub const MAX_REPORT_BYTES: usize = 1024 * 1024;
/// Maximum UTF-8 input per APPEND trap. Clients stream larger reports in chunks.
pub const MAX_APPEND_BYTES: usize = 512;
pub const TRUNCATED: u32 = 1;
pub const FORMAT_FAILED: u32 = 2;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct CrashInfo
{
    pub window:       WindowId,
    pub report_bytes: usize,
    pub line_count:   u32,
    pub first_line:   u32,
    pub page_lines:   u32,
    pub dismissed:    u32,
    pub flags:        u32,
    pub app:          u64,
    pub thread:       u64
}
#[derive(Default)]
#[repr(C)]
pub struct CrashCall
{
    pub operation: u32,
    pub flags:     u32,
    pub data:      *const u8,
    pub length:    usize,
    pub offset:    usize,
    pub output:    *mut u8,
    pub capacity:  usize,
    pub delta:     i32,
    pub info:      CrashInfo
}
