//! Byte-transport pipes. A pipe carries bytes in and bytes out; it has no
//! framing, record boundary or message operation. Handles are opaque,
//! generation-tagged and owned by the process that received them.
pub const CREATE: u32 = 0;
pub const CREATE_NAMED: u32 = 1;
pub const CONNECT: u32 = 2;
pub const READ: u32 = 3;
pub const WRITE: u32 = 4;
pub const SET_BLOCKING: u32 = 5;
pub const CLOSE: u32 = 6;
/// Read with a per-call timeout, independently of the endpoint's blocking mode.
/// Ready bytes and real EOF take priority over expiry. Expiry returns WouldBlock
/// without modifying the output buffer; it never manufactures EOF.
pub const READ_WAIT: u32 = 7;
/// Create another owned endpoint to the same pipe, with independent blocking
/// mode. Closing an alias does not close the stream while other aliases live.
/// Returns the new identity in reader or writer according to its direction.
pub const DUPLICATE: u32 = 8;
/// Associate three caller-owned anonymous pipes with terminal discipline.
/// `handle` is the frontend input writer; `terminal_outputs` are its stdout
/// and stderr readers. No additional session identity is created.
pub const TERMINAL_ATTACH: u32 = 9;
/// Query terminal state through any associated endpoint. Ordinary pipes
/// return NotSupported; ownership and generation checks still apply.
pub const TERMINAL_QUERY: u32 = 10;
/// Set mode through the child's input reader. Closing the setting process's last input alias
/// restores the previous live mode (cooked/echo by default). Unknown flags are refused.
pub const TERMINAL_MODE: u32 = 11;
/// Publish the frontend's new dimensions. Revision changes only on a change.
pub const TERMINAL_RESIZE: u32 = 12;
/// Deliver keyboard/paste bytes through the frontend input writer. Ordinary
/// WRITE remains byte-transparent, including terminal protocol replies.
pub const TERMINAL_INPUT: u32 = 13;
/// Like READ_WAIT, but also returns when terminal.revision differs from the
/// input revision. A changed revision with transferred == 0 is a state change,
/// not EOF; the returned terminal descriptor carries the new geometry/mode.
pub const TERMINAL_READ_WAIT: u32 = 14;
/// Request a cooked-mode interrupt independently of input backpressure.
/// Raw/no-interrupt mode returns NotSupported so the frontend can queue Ctrl+C
/// as ordinary input. Only the frontend input writer can request this.
pub const TERMINAL_INTERRUPT: u32 = 15;
pub const TERMINAL_RAW: u32 = 1;
pub const TERMINAL_ECHO: u32 = 2;
pub const TERMINAL_INTERRUPTS: u32 = 4;
pub const TERMINAL_DEFAULT: u32 = TERMINAL_ECHO | TERMINAL_INTERRUPTS;
pub const TERMINAL_FLAGS: u32 = TERMINAL_RAW | TERMINAL_ECHO | TERMINAL_INTERRUPTS;
/// Resident editing storage budget, independent of the transport ring size.
pub const TERMINAL_LINE_BYTES: usize = 65_536;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalInfo
{
    pub flags:    u32,
    pub columns:  u16,
    pub rows:     u16,
    pub width:    u16,
    pub height:   u16,
    pub reserved: u32,
    pub revision: u64
}

/// Capacity bounds. The lower bound keeps a maximum-size atomic write able to
/// fit; the upper bound is the v1 storage policy.
pub const MIN_CAPACITY: usize = 512;
pub const MAX_CAPACITY: usize = 65_536;
/// Resident pipe limit, shared by anonymous and named pipes.
pub const MAX_PIPES: usize = 64;
/// A write of at most this size is indivisible against concurrent writers.
/// It is an atomicity bound only and is never a record size.
pub const ATOMIC_WRITE: usize = 512;
/// HFS item-name bound, because a named pipe is also a Finder item.
pub const NAME_BYTES: usize = 31;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Call
{
    pub operation:        u32,
    /// Zero is blocking; one is non-blocking. Initial mode on create, new mode on `SET_BLOCKING`.
    pub nonblocking:      u32,
    pub handle:           u64,
    pub capacity:         u32,
    pub name_length:      u32,
    pub buffer:           *mut u8,
    pub length:           usize,
    pub transferred:      usize,
    pub reader:           u64,
    pub writer:           u64,
    pub name:             [u8; NAME_BYTES],
    pub reserved:         [u64; 1],
    /// READ_WAIT only: relative nanoseconds; u64::MAX waits indefinitely.
    /// Zero tries once. Other operations require zero.
    pub wait_nanoseconds: u64,
    /// Maximum concurrent `connect` writers for a named pipe; zero is
    /// unlimited. The creator's own writer handle is not a connection and is
    /// not counted, so a limit of one means one connecting owner at a time.
    /// Only `CREATE_NAMED` accepts a nonzero value.
    pub writers:          u64,
    pub terminal_outputs: [u64; 2],
    pub terminal:         TerminalInfo
}
impl Default for Call
{
    fn default() -> Self
    {
        Self { operation:        0,
               nonblocking:      0,
               handle:           0,
               capacity:         0,
               name_length:      0,
               buffer:           core::ptr::null_mut(),
               length:           0,
               transferred:      0,
               reader:           0,
               writer:           0,
               name:             [0; NAME_BYTES],
               reserved:         [0; 1],
               wait_nanoseconds: 0,
               writers:          0,
               terminal_outputs: [0; 2],
               terminal:         TerminalInfo::default() }
    }
}
#[cfg(target_pointer_width = "64")]
const _: () = assert!(core::mem::size_of::<Call>() == 160
                      && core::mem::align_of::<Call>() == 8
                      && core::mem::offset_of!(Call, handle) == 8
                      && core::mem::offset_of!(Call, capacity) == 16
                      && core::mem::offset_of!(Call, buffer) == 24
                      && core::mem::offset_of!(Call, length) == 32
                      && core::mem::offset_of!(Call, transferred) == 40
                      && core::mem::offset_of!(Call, reader) == 48
                      && core::mem::offset_of!(Call, writer) == 56
                      && core::mem::offset_of!(Call, name) == 64
                      && core::mem::offset_of!(Call, reserved) == 96
                      && core::mem::offset_of!(Call, wait_nanoseconds) == 104
                      && core::mem::offset_of!(Call, writers) == 112
                      && core::mem::offset_of!(Call, terminal_outputs) == 120
                      && core::mem::offset_of!(Call, terminal) == 136);
