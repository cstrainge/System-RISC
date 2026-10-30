//! Native transport numbers. Classic A-line numbers occupy their original u16 range.
use crate::*;

/// System-owned Standard File dialogs; native range beyond every classic 16-bit trap.
pub const SCROLL_BOX: u32 = 0x10021;
pub const DRAG: u32 = 0x10022;
/// Additive event delivery with resident-captured window coordinates.
pub const POSITIONED_EVENT: u32 = 0x10023;
/// Public capability-checked domain inspection; outside the classic trap range.
pub const INSPECTION: u32 = 0x10024;
pub const THEME: u32 = 0x10025;
/// Definition-runtime fault report; admitted only during a host-owned invocation.
pub const DEFINITION_PANIC: u32 = 0x10026;
pub const POWER: u32 = 0x10027;
/// Public CPU discovery/activity, independent of inspector and scheduler grants.
pub const APPLICATION_HEAP: u32 = 0x10028;
pub const CPU_INFO: u32 = 0x10029;
pub const PSTRING_SCROLL_BOX: u32 = SCROLL_BOX | PSTRING_BIT;
pub const STANDARD_FILE: u32 = 0x10020;
pub const PSTRING_STANDARD_FILE: u32 = STANDARD_FILE | PSTRING_BIT;
pub const CRASH: u32 = 0x1001f;
pub const SCRAP: u32 = 0x10019;
pub const EVENTS: u32 = 0x1001a;
pub const TEXTEDIT: u32 = 0x1001b;
pub const CLOCK: u32 = 0x10018;
/// Native monotonic boot clock; independent of wall-clock dates and 60-Hz TickCount.
pub const MONOTONIC_CLOCK: u32 = 0x1001e;
pub const DIALOG: u32 = 0x10017;
pub const MENU: u32 = 0x10016;
pub const LOG: u32 = 0x10000;
pub const IMAGE: u32 = 0x10015;
pub const WINDOW: u32 = 0x10001;
pub const DRAW: u32 = 0x10002;
pub const NEXT_EVENT: u32 = 0x10003;
/// Native scheduler-backed Event Manager wait; existing NEXT_EVENT stays nonblocking.
pub const WAIT_EVENT: u32 = 0x1001d;
pub const DEFINITION: u32 = 0x10004;
pub const CLASSIC: u32 = 0x10005;
pub const GEOMETRY: u32 = 0x10006;
pub const SURFACE: u32 = 0x10007;
pub const QUICKDRAW: u32 = 0x10008;
pub const FONT: u32 = 0x10009;
pub const REGION: u32 = 0x1000a;
pub const COPY_BITS: u32 = 0x1000b;
pub const WORLD: u32 = 0x1000c;
pub const FILE: u32 = 0x10012;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LogCall
{
    pub bytes:  *const u8,
    pub length: usize
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct WindowCall
{
    pub operation: u32,
    pub request:   WindowRequest
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DrawCall
{
    pub window:  u64,
    pub command: DrawCommand
}
#[repr(C)]
pub struct NextEventCall
{
    pub mask:  u16,
    pub event: Event
}
#[repr(C)]
#[derive(Default)]
pub struct WaitEventCall
{
    pub mask:          u16,
    /// Maximum wait in 60-Hz uptime ticks; zero performs a nonblocking poll.
    pub timeout_ticks: u32,
    pub event:         Event,
    /// Resident transport flag: the caller slept and must poll after resumption.
    pub waited:        u32
}
#[repr(C)]
#[derive(Default)]
pub struct GeometryCall
{
    pub operation: u32,
    pub a:         Rect,
    pub b:         Rect,
    pub output:    Rect,
    pub result:    u32
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClassicRegisters
{
    pub d:        [u32; 8],
    pub a:        [u32; 8],
    pub pc:       u32,
    pub sr:       u16,
    pub reserved: u16
}
#[repr(C)]
pub struct ClassicCall
{
    pub instruction:   u16,
    pub reserved:      u16,
    pub guest_base:    u32,
    pub memory:        *mut u8,
    pub memory_length: usize,
    pub registers:     ClassicRegisters
}

/// Original identities, verified against the pinned Multiversal interface headers.
pub mod classic;

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct SurfaceCall
{
    pub operation:   u32,
    pub surface:     *mut Surface,
    pub bounds:      Rect,
    pub foreground:  u32,
    pub background:  u32,
    pub pattern:     [u8; 8],
    pub font:        u32,
    pub x:           i16,
    pub y:           i16,
    pub limit:       i16,
    pub reserved:    i16,
    pub text:        *const u8,
    pub text_length: usize,
    pub width:       i16,
    pub ascent:      i16,
    pub descent:     i16,
    pub leading:     i16
}

pub const CURSOR: u32 = 0x1000e;

pub const MAPPING: u32 = 0x1000f;

pub const RESOURCE: u32 = 0x10010;

pub const PICTURE: u32 = 0x1000d;
pub const ICON: u32 = 0x10011;

pub const CONTROL: u32 = 0x10013;
pub const MEMORY: u32 = 0x10014;

/// Pascal/MacRoman counterparts of every native service carrying human text.
/// Wire structs are identical; string lengths include the Pascal length byte.
/// These are legacy ABI entry points, deliberately absent from Rust SDK clients.
pub const PSTRING_BIT: u32 = 0x20000;
pub const PSTRING_PROCESS: u32 = PROCESS | PSTRING_BIT;
pub const PSTRING_LOG: u32 = LOG | PSTRING_BIT;
pub const PSTRING_WINDOW: u32 = WINDOW | PSTRING_BIT;
pub const PSTRING_DRAW: u32 = DRAW | PSTRING_BIT;
pub const PSTRING_SURFACE: u32 = SURFACE | PSTRING_BIT;
pub const PSTRING_FONT: u32 = FONT | PSTRING_BIT;
pub const PSTRING_RESOURCE: u32 = RESOURCE | PSTRING_BIT;
pub const PSTRING_FILE: u32 = FILE | PSTRING_BIT;
pub const PSTRING_TEXTEDIT: u32 = TEXTEDIT | PSTRING_BIT;
pub const PSTRING_SCRAP: u32 = SCRAP | PSTRING_BIT;
pub const PSTRING_DIALOG: u32 = DIALOG | PSTRING_BIT;
pub const PSTRING_CONTROL: u32 = CONTROL | PSTRING_BIT;
pub const fn native_string_trap(number: u32) -> Option<u32>
{
    match number
    {
        PSTRING_SCROLL_BOX
        | PSTRING_LOG
        | PSTRING_WINDOW
        | PSTRING_DRAW
        | PSTRING_SURFACE
        | PSTRING_FONT
        | PSTRING_STANDARD_FILE
        | PSTRING_RESOURCE
        | PSTRING_FILE
        | PSTRING_CONTROL
        | PSTRING_DIALOG
        | PSTRING_SCRAP
        | PSTRING_TEXTEDIT
        | PSTRING_PROCESS => Some(number & !PSTRING_BIT),
        _ => None
    }
}

pub const PROCESS: u32 = 0x1001c;

/// Native application Thread Manager (separate from classic co-operative threads).
pub const NATIVE_THREAD: u32 = 0x1002a;
pub const TLS: u32 = 0x1002b;

pub const ADDRESS_WAIT: u32 = 0x1002c;
