//! Native process clients. The resident scheduler owns execution and lifecycle.
#![no_std]
#![forbid(unsafe_code)]
mod documents;
pub use documents::{DocumentBatch, DocumentBatchId, DocumentOpen, DocumentRef};
use mac_traps::types::InlineText;
pub use mac_traps::{Cnid, ProcessId, ThreadId, VolumeId};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcessInfo
{
    pub id:         ProcessId,
    pub thread:     ThreadId,
    pub volume:     VolumeId,
    pub cnid:       Cnid,
    pub foreground: bool,
    pub hidden:     bool,
    pub name:       InlineText<{ mac_abi::INLINE_STRING_BYTES }>,
    pub switches:   u64
}
impl ProcessInfo
{
    pub fn name(&self) -> &str
    {
        self.name.as_str()
    }
}
impl TryFrom<mac_abi::processes::ProcessInfo> for ProcessInfo
{
    type Error = mac_traps::Error;

    fn try_from(info: mac_abi::processes::ProcessInfo) -> Result<Self>
    {
        Ok(Self { id:         ProcessId(info.id),
                  thread:     ThreadId(info.thread),
                  volume:     VolumeId(info.volume),
                  cnid:       Cnid(info.cnid),
                  foreground: info.foreground != 0,
                  hidden:     info.hidden != 0,
                  name:       InlineText::from_reply(info.name, info.name_length)?,
                  switches:   info.switches })
    }
}
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
        self.call(ProcessCall { operation: CURRENT,
                                ..Default::default() })?
            .info
            .try_into()
    }

    pub fn count(self) -> Result<u32>
    {
        Ok(self.call(ProcessCall { operation: COUNT,
                                   ..Default::default() })?
               .index)
    }

    pub fn at(self, index: u32) -> Result<ProcessInfo>
    {
        self.call(ProcessCall { operation: INDEX,
                                index,
                                ..Default::default() })?
            .info
            .try_into()
    }

    /// Whether this exact file has a live instance, including hidden/background apps.
    pub fn is_running(self, volume: VolumeId, cnid: Cnid) -> Result<bool>
    {
        Ok(self.call(ProcessCall { operation: IS_RUNNING,
                                   volume: volume.0,
                                   cnid: cnid.0,
                                   ..Default::default() })?
               .index
           != 0)
    }

    pub fn launch(self, volume: VolumeId, cnid: Cnid) -> Result<ProcessInfo>
    {
        self.call(ProcessCall { operation: LAUNCH,
                                volume: volume.0,
                                cnid: cnid.0,
                                ..Default::default() })?
            .info
            .try_into()
    }

    pub fn activate(self, id: ProcessId) -> Result<()>
    {
        self.call(ProcessCall { operation: ACTIVATE,
                                id: id.0,
                                ..Default::default() })
            .map(|_| ())
    }

    pub fn hide(self, id: ProcessId) -> Result<()>
    {
        self.call(ProcessCall { operation: HIDE,
                                id: id.0,
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
