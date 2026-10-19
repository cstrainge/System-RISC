//! Resident monochrome TextEdit, with checked UTF-8 byte offsets for native apps.
use crate::{Point, Rect, WindowId};
pub const CREATE: u32 = 0;
pub const DISPOSE: u32 = 1;
pub const INFO: u32 = 2;
pub const TEXT: u32 = 3;
pub const SET_TEXT: u32 = 4;
pub const INSERT: u32 = 5;
pub const DELETE: u32 = 6;
pub const KEY: u32 = 7;
pub const SELECT: u32 = 8;
pub const LINE: u32 = 9;
pub const OFFSET: u32 = 10;
pub const POINT: u32 = 11;
pub const SCROLL: u32 = 12;
pub const ACTIVATE: u32 = 13;
pub const SELECTION_TEXT: u32 = 14;
pub const DRAW: u32 = 15;
pub const IDLE: u32 = 16;
pub const CLICK: u32 = 17;
pub const COPY: u32 = 18;
pub const CUT: u32 = 19;
pub const PASTE: u32 = 20;
pub const SET_RECTS: u32 = 21;
/// Native cursor movement; `key` is CursorMovement and `index` is extend (0/1).
pub const NAVIGATE: u32 = 22;
/// Size-distinct native font request; the existing TextEditCall stays ABI-stable.
pub const SET_FONT: u32 = 23;
/// Enable automatic I-beam hover independently of keyboard activation.
/// Uses TextEditCall.index as a checked boolean (0/1).
pub const SET_CURSOR_ENABLED: u32 = 24;
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TextEditFontCall
{
    pub operation: u32,
    pub id:        EditorId,
    pub font:      crate::quickdraw::FontSelection,
    pub info:      EditorInfo
}
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorMovement
{
    Left          = 0,
    Right         = 1,
    Up            = 2,
    Down          = 3,
    LineStart     = 4,
    LineEnd       = 5,
    DocumentStart = 6,
    DocumentEnd   = 7
}
impl CursorMovement
{
    pub const fn from_raw(value: u32) -> Option<Self>
    {
        match value
        {
            0 => Some(Self::Left),
            1 => Some(Self::Right),
            2 => Some(Self::Up),
            3 => Some(Self::Down),
            4 => Some(Self::LineStart),
            5 => Some(Self::LineEnd),
            6 => Some(Self::DocumentStart),
            7 => Some(Self::DocumentEnd),
            _ => None
        }
    }
}
pub const MAX_BYTES: usize = 32767;
pub const MAX_EDITORS: usize = 64;
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditorId(pub u32);
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditorInfo
{
    pub dest:            Rect,
    pub view:            Rect,
    pub selection_start: u32,
    pub selection_end:   u32,
    pub text_length:     u32,
    pub line_count:      u32,
    pub line_height:     i16,
    pub ascent:          i16,
    pub active:          u16,
    pub caret_visible:   u16,
    pub revision:        u32
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LineInfo
{
    pub start:    u32,
    /// Includes a terminating carriage return, when present.
    pub end:      u32,
    pub width:    i16,
    pub reserved: u16
}
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TextEditCall
{
    pub operation:    u32,
    pub id:           EditorId,
    pub window:       WindowId,
    pub info:         EditorInfo,
    pub line:         LineInfo,
    pub index:        u32,
    pub point:        Point,
    pub delta:        Point,
    /// Unicode scalar for KEY; 8 backspace, 28..31 classic cursor directions.
    /// CursorMovement discriminant for NAVIGATE.
    pub key:          u32,
    /// Wrapping 60 Hz tick count for IDLE.
    pub ticks:        u32,
    pub input:        *const u8,
    pub input_length: usize,
    pub output:       *mut u8,
    pub capacity:     usize,
    pub transferred:  usize
}
impl Default for TextEditCall
{
    fn default() -> Self
    {
        Self { operation:    0,
               id:           EditorId(0),
               window:       WindowId(0),
               info:         EditorInfo::default(),
               line:         LineInfo::default(),
               index:        0,
               point:        Point::default(),
               delta:        Point::default(),
               key:          0,
               ticks:        0,
               input:        core::ptr::null(),
               input_length: 0,
               output:       core::ptr::null_mut(),
               capacity:     0,
               transferred:  0 }
    }
}
