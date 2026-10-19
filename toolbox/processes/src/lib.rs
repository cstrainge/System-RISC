//! Native process clients. The resident scheduler owns execution and lifecycle.
#![no_std]
#![forbid(unsafe_code)]
pub use mac_abi::processes::ProcessInfo;
use mac_abi::processes::*;
pub use mac_traps::{Client, Result};
#[derive(Clone, Copy)]
pub struct Processes<'a>
{
    client: Client<'a>
}
impl<'a> Processes<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    fn call(self, mut call: ProcessCall) -> Result<ProcessCall>
    {
        self.client.process(&mut call)?;
        Ok(call)
    }

    pub fn current(self) -> Result<ProcessInfo>
    {
        Ok(self.call(ProcessCall { operation: CURRENT,
                                   ..Default::default() })?
               .info)
    }

    pub fn count(self) -> Result<u32>
    {
        Ok(self.call(ProcessCall { operation: COUNT,
                                   ..Default::default() })?
               .index)
    }

    pub fn at(self, index: u32) -> Result<ProcessInfo>
    {
        Ok(self.call(ProcessCall { operation: INDEX,
                                   index,
                                   ..Default::default() })?
               .info)
    }

    /// Whether this exact file has a live instance, including hidden/background apps.
    pub fn is_running(self, volume: u32, cnid: u32) -> Result<bool>
    {
        Ok(self.call(ProcessCall { operation: IS_RUNNING,
                                   volume,
                                   cnid,
                                   ..Default::default() })?
               .index
           != 0)
    }

    pub fn launch(self, volume: u32, cnid: u32) -> Result<ProcessInfo>
    {
        Ok(self.call(ProcessCall { operation: LAUNCH,
                                   volume,
                                   cnid,
                                   ..Default::default() })?
               .info)
    }

    pub fn activate(self, id: u64) -> Result<()>
    {
        self.call(ProcessCall { operation: ACTIVATE,
                                id,
                                ..Default::default() })
            .map(|_| ())
    }

    pub fn hide(self, id: u64) -> Result<()>
    {
        self.call(ProcessCall { operation: HIDE,
                                id,
                                ..Default::default() })
            .map(|_| ())
    }

    pub fn hide_others(self) -> Result<()>
    {
        self.call(ProcessCall { operation: HIDE_OTHERS,
                                ..Default::default() })
            .map(|_| ())
    }

    pub fn show_all(self) -> Result<()>
    {
        self.call(ProcessCall { operation: SHOW_ALL,
                                ..Default::default() })
            .map(|_| ())
    }
}
