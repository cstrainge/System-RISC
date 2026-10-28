//! Client for System-owned, scrollable native panic diagnostics.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::crashes::*;
pub use mac_abi::crashes::{MAX_APPEND_BYTES, MAX_REPORT_BYTES};
pub use mac_traps::types::{ProcessId, ThreadId};
mac_traps::native_flags!(ReportFlags, u32; TRUNCATED = mac_abi::crashes::TRUNCATED, FORMAT_FAILED = mac_abi::crashes::FORMAT_FAILED);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CrashInfo
{
    pub window:       mac_abi::WindowId,
    pub report_bytes: usize,
    pub line_count:   u32,
    pub first_line:   u32,
    pub page_lines:   u32,
    pub dismissed:    bool,
    pub flags:        ReportFlags,
    pub app:          ProcessId,
    pub thread:       ThreadId
}
impl From<mac_abi::crashes::CrashInfo> for CrashInfo
{
    fn from(info: mac_abi::crashes::CrashInfo) -> Self
    {
        Self { window:       info.window,
               report_bytes: info.report_bytes,
               line_count:   info.line_count,
               first_line:   info.first_line,
               page_lines:   info.page_lines,
               dismissed:    info.dismissed != 0,
               flags:        ReportFlags::from_bits_retain(info.flags),
               app:          ProcessId(info.app),
               thread:       ThreadId(info.thread) }
    }
}
pub use mac_traps::{Client, Error, Result};
#[derive(Clone, Copy)]
pub struct Crashes<'a>
{
    client: Client<'a>
}
impl<'a> Crashes<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    pub fn begin(self) -> Result<CrashInfo>
    {
        self.command(BEGIN, 0, 0)
    }

    /// Stream UTF-8 in bounded resident calls without allocating. An argument
    /// larger than the report retention limit is refused before any effect.
    /// If a later chunk refuses, previously accepted chunks remain retained.
    pub fn append(self, mut text: &str) -> Result<CrashInfo>
    {
        if text.len() > MAX_REPORT_BYTES
        {
            return Err(Error::BadArgument);
        }
        loop
        {
            let mut count = text.len().min(MAX_APPEND_BYTES);
            while !text.is_char_boundary(count)
            {
                count -= 1;
            }
            let mut call = CrashCall { operation: APPEND,
                                       ..Default::default() };
            self.client.crash(&mut call, &text[..count], &mut [])?;
            if count == text.len()
            {
                return Ok(call.info.into());
            }
            text = &text[count..];
        }
    }

    pub fn show(self, flags: ReportFlags) -> Result<CrashInfo>
    {
        self.command(SHOW, flags.bits(), 0)
    }

    pub fn info(self) -> Result<CrashInfo>
    {
        self.command(INFO, 0, 0)
    }

    /// Read exact UTF-8 bytes. A chunk may end inside a scalar; combine bytes before decoding.
    pub fn read(self, offset: usize, output: &mut [u8]) -> Result<usize>
    {
        let mut call = CrashCall { operation: READ,
                                   offset,
                                   ..Default::default() };
        self.client.crash(&mut call, "", output)?;
        Ok(call.length)
    }

    pub fn scroll(self, delta: i32) -> Result<CrashInfo>
    {
        self.command(SCROLL, 0, delta)
    }

    pub fn dismiss(self) -> Result<CrashInfo>
    {
        self.command(DISMISS, 0, 0)
    }

    fn command(self, operation: u32, flags: u32, delta: i32) -> Result<CrashInfo>
    {
        let mut call = CrashCall { operation,
                                   flags,
                                   delta,
                                   ..Default::default() };
        self.client.crash(&mut call, "", &mut [])?;
        Ok(call.info.into())
    }
}
