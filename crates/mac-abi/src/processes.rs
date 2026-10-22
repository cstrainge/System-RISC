//! Process identities are assigned by the scheduler, never supplied as authority.
use crate::*;
pub const CURRENT: u32 = 0;
pub const COUNT: u32 = 1;
pub const INDEX: u32 = 2;
pub const LAUNCH: u32 = 3;
pub const ACTIVATE: u32 = 4;
pub const HIDE: u32 = 5;
pub const HIDE_OTHERS: u32 = 6;
pub const SHOW_ALL: u32 = 7;
pub const EXIT: u32 = 8;
/// Exact mounted-volume/catalog identity, independent of path or creator code.
pub const IS_RUNNING: u32 = 9;
/// Native lifecycle notification: payload packs volume low/CNID high; message
/// low bit reports whether this exact file still has a running instance.
pub const FILE_STATE_EVENT: u32 = 0x0c00_0000;
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ProcessInfo
{
    pub id:          u64,
    pub thread:      u64,
    pub volume:      u32,
    pub cnid:        u32,
    pub foreground:  u32,
    pub hidden:      u32,
    pub name:        [u8; INLINE_STRING_BYTES],
    pub name_length: u32,
    pub switches:    u64
}
impl Default for ProcessInfo
{
    fn default() -> Self
    {
        Self { id:          0,
               thread:      0,
               volume:      0,
               cnid:        0,
               foreground:  0,
               hidden:      0,
               name:        [0; INLINE_STRING_BYTES],
               name_length: 0,
               switches:    0 }
    }
}
impl ProcessInfo
{
    pub fn name(&self) -> &str
    {
        core::str::from_utf8(&self.name[..(self.name_length as usize).min(self.name.len())]).unwrap_or("")
    }
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct ProcessCall
{
    pub operation: u32,
    pub index:     u32,
    pub id:        u64,
    pub volume:    u32,
    pub cnid:      u32,
    pub info:      ProcessInfo
}
