//! Client for System-owned, scrollable native panic diagnostics.
#![no_std]
#![forbid(unsafe_code)]
pub use mac_abi::crashes::*;
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

    pub fn append(self, text: &str) -> Result<CrashInfo>
    {
        let mut call = CrashCall { operation: APPEND,
                                   ..Default::default() };
        self.client.crash(&mut call, text, &mut [])?;
        Ok(call.info)
    }

    pub fn show(self, flags: u32) -> Result<CrashInfo>
    {
        self.command(SHOW, flags, 0)
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
        Ok(call.info)
    }
}
