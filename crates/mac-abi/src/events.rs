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
    Null         = 0,
    MouseDown    = 1,
    MouseUp      = 2,
    KeyDown      = 3,
    KeyUp        = 4,
    AutoKey      = 5,
    Update       = 6,
    Disk         = 7,
    Activate     = 8,
    /// Captured low-level OS queue accepts this otherwise reserved event code.
    Reserved9    = 9,
    Network      = 10,
    Driver       = 11,
    Application1 = 12,
    Application2 = 13,
    /// Native menu command: high16 menu ID, low16 one-based item.
    /// In the separate classic OS queue14 remains an application-defined event.
    MenuCommand  = 14,
    OsEvent      = 15
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

/// Physical Macintosh virtual-key state. Bit `code % 8` of byte `code / 8`
/// corresponds to that virtual key; character encoding and keyboard layout do
/// not change the bit index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct KeyMap(pub [u8; 16]);
impl KeyMap
{
    pub fn is_down(self, code: u8) -> bool
    {
        code < 128 && self.0[code as usize / 8] & (1 << (code % 8)) != 0
    }

    pub fn set(&mut self, code: u8, down: bool)
    {
        if code < 128
        {
            let byte = &mut self.0[code as usize / 8];
            let mask = 1 << (code % 8);
            if down
            {
                *byte |= mask;
            }
            else
            {
                *byte &= !mask;
            }
        }
    }
}

pub const NATIVE_POST: u32 = 0;
pub const NATIVE_PEEK: u32 = 1;
pub const NATIVE_FLUSH: u32 = 2;
pub const OS_POST: u32 = 3;
pub const OS_PEEK: u32 = 4;
pub const OS_GET: u32 = 5;
pub const OS_FLUSH: u32 = 6;
pub const INPUT_STATE: u32 = 7;
pub const OS_SET_MASK: u32 = 8;
pub const OS_GET_MASK: u32 = 9;
pub const OS_STILL_DOWN: u32 = 10;
/// Original `evtNotEnb`: posting is disabled by the low-level event mask.
pub const EVENT_NOT_ENABLED: i32 = 1;

/// Pointer-free Event Manager request. Native application events and the
/// classic low-level OS queue are deliberately distinct services.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct EventCall
{
    pub operation:    u32,
    pub mask:         u16,
    pub stop_mask:    u16,
    pub event:        Event,
    pub present:      u32,
    pub queued:       u32,
    pub keys:         KeyMap,
    pub ticks:        u32,
    /// Native extension: unwrapped 60-Hz ticks since machine startup.
    pub uptime_ticks: u64
}
