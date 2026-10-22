//! Shared Drag Manager transport. Native and classic boundaries use one store.
use crate::{Point, Rect};
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DragId(pub u32);
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReceiverId(pub u32);
pub const DISPOSE: u32 = 1;
pub const INFO: u32 = 2;
pub const ADD_ITEM: u32 = 3;
pub const OFFER: u32 = 4;
pub const CANCEL: u32 = 6;
pub const ACCEPT: u32 = 9;
pub const ITEM_AT: u32 = 10;
pub const FLAVOR_AT: u32 = 11;
pub const READ: u32 = 12;
pub const FULFILL: u32 = 13;
pub const NEXT_PROMISE: u32 = 14;
pub const ACKNOWLEDGE: u32 = 15;
pub const CREATE: u32 = 16;
pub const BEGIN: u32 = 17;
pub const CLASSIC_ADD: u32 = 18;
pub const CLASSIC_SET: u32 = 19;
pub const CLASSIC_FLAVOR: u32 = 20;
pub const CLASSIC_READ: u32 = 21;
pub const ITEM_BOUNDS: u32 = 22;
pub const FILE_ITEMS: u32 = 24;
/// RAII rejection only; validates receiver ownership but permits cleanup on any thread.
pub const ABANDON: u32 = 25;
pub const CLASSIC_COUNT: u32 = 23;
pub const DRAFT: u32 = 6;
pub const COPY: u32 = 1;
pub const MOVE: u32 = 2;
pub const COPIED: u32 = 5;
pub const MOVED: u32 = 6;
pub const TARGET_TRASH: u32 = 2;
/// Receiver intent. At least one action is required on an accepting response.
pub const ACCEPT_COPY: u32 = 4;
pub const ACCEPT_MOVE: u32 = 8;
/// Resident File Manager admission: its atomic catalog move retains a volume.
pub const SAME_VOLUME_MOVE: u32 = 16;
pub const FLAVOR_UNAVAILABLE: i32 = -1852;
pub const PROMISE_FAILED: i32 = -1854;
pub const MAX_ITEMS: usize = 1024;
pub const MAX_FLAVORS: usize = 8;
pub const MAX_BYTES: usize = 64 * 1024;
/// fref: LE volume/CNID/expected parent, LE origin h/v, BE QuickDraw bounds.
pub const FILE_ITEM_BYTES: usize = 24;
pub const PROMISED: u32 = 1;
pub const ARMED: u32 = 0;
pub const TRACKING: u32 = 2;
pub const DROPPING: u32 = 3;
pub const RETURNING: u32 = 4;
pub const FINISHED: u32 = 5;
pub const CLICKED: u32 = 1;
pub const CONSUMED: u32 = 2;
pub const REJECTED: u32 = 3;
pub const CANCELLED: u32 = 4;
pub const ENTERED_EVENT: u32 = 0x0500_0000;
pub const WITHIN_EVENT: u32 = 0x0600_0000;
pub const LEFT_EVENT: u32 = 0x0700_0000;
pub const DROP_EVENT: u32 = 0x0800_0000;
pub const PROMISE_EVENT: u32 = 0x0900_0000;
pub const DATA_READY_EVENT: u32 = 0x0b00_0000;
pub const FINISHED_EVENT: u32 = 0x0a00_0000;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Info
{
    pub phase:     u32,
    pub outcome:   u32,
    pub items:     u32,
    pub receiver:  u32,
    pub window:    u32,
    pub point:     Point,
    pub modifiers: u16,
    pub action:    u16,
    pub error:     i32
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Call
{
    pub operation:   u32,
    pub id:          u32,
    pub receiver:    u32,
    pub window:      u32,
    pub item:        u32,
    pub index:       u32,
    pub volume:      u32,
    pub parent:      u32,
    pub kind:        [u8; 4],
    pub flags:       u32,
    pub error:       i32,
    pub point:       Point,
    pub bounds:      Rect,
    pub info:        Info,
    pub input:       *const u8,
    pub length:      usize,
    pub output:      *mut u8,
    pub capacity:    usize,
    pub transferred: usize
}
