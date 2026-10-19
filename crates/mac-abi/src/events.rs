//! A bounded Event Manager subset with classic event numbers and a 68k codec.
use crate::Point;

pub const EVERY_EVENT: u16 = 0xffff;
pub const ACTIVE_FLAG: u16 = 1;
pub const BTN_STATE: u16 = 128;
pub const CMD_KEY: u16 = 256;
pub const SHIFT_KEY: u16 = 512;
pub const ALPHA_LOCK: u16 = 1024;
pub const OPTION_KEY: u16 = 2048;
pub const CONTROL_KEY: u16 = 4096;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u16)]
pub enum EventKind
{
    #[default]
    Null        = 0,
    MouseDown   = 1,
    MouseUp     = 2,
    KeyDown     = 3,
    KeyUp       = 4,
    AutoKey     = 5,
    Update      = 6,
    Activate    = 8,
    /// Native menu command: high16 menu ID, low16 one-based item.
    MenuCommand = 14,
    OsEvent     = 15
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Event
{
    pub kind:      EventKind,
    pub message:   u32,
    pub when:      u32,
    pub where_:    Point,
    pub modifiers: u16
}
impl Event
{
    pub fn matches(self, mask: u16) -> bool
    {
        mask & (1 << self.kind as u16) != 0
    }

    pub fn character(self) -> u8
    {
        self.message as u8
    }

    /// Macintosh EventRecord uses two-byte alignment, not Rust's native padding.
    pub fn to_be_bytes(self) -> [u8; 16]
    {
        let mut bytes = [0; 16];
        bytes[0..2].copy_from_slice(&(self.kind as u16).to_be_bytes());
        bytes[2..6].copy_from_slice(&self.message.to_be_bytes());
        bytes[6..10].copy_from_slice(&self.when.to_be_bytes());
        bytes[10..12].copy_from_slice(&self.where_.v.to_be_bytes());
        bytes[12..14].copy_from_slice(&self.where_.h.to_be_bytes());
        bytes[14..16].copy_from_slice(&self.modifiers.to_be_bytes());
        bytes
    }
}
