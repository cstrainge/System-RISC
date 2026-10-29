//! Native Control Manager requests. CDEF resources own control geometry and pixels.
pub mod reflow;
use crate::Rect;
pub const DRAW_SCROLLBAR: u32 = 0;
pub const HIT_SCROLLBAR: u32 = 1;
pub const TRACK_SCROLLBAR: u32 = 2;
pub const DRAW_STANDARD: u32 = 3;
pub const HIT_STANDARD: u32 = 4;
pub const CREATE_STANDARD: u32 = 10;
pub const CREATE_SCROLLBAR: u32 = 11;
pub const CONFIGURE_OBJECT: u32 = 12;
pub const INFO_OBJECT: u32 = 13;
pub const DISPOSE_OBJECT: u32 = 14;
pub const DRAW_OBJECT: u32 = 15;
pub const BEGIN_TRACKING: u32 = 16;
pub const POLL_TRACKING: u32 = 17;
pub const CANCEL_TRACKING: u32 = 18;
pub const REFLOW_OBJECT: u32 = 19;
pub const BUTTON_DOWN: u32 = 1;
pub const TRACKING: u32 = 2;
pub const HANDLED: u32 = 4;
pub const ACTION: u32 = 8;
pub const CHANGED: u32 = 16;
/// Native CDEF state validation before retained drawing; no surface required.
pub const VALIDATE_DEFINITION: i16 = 5;
/// Native-only passive control procedures. These are not classic procIDs.
pub const PROGRESS_BAR_PROCEDURE: i16 = i16::MIN;
pub const STATIC_TEXT_PROCEDURE: i16 = i16::MIN + 1;
pub const MONOCHROME_PROGRESS_BAR_PROCEDURE: i16 = i16::MIN + 2;
pub const CDEF_PROGRESS_BAR: i16 = -32760;
pub const CDEF_STATIC_TEXT: i16 = -32759;
/// Ask a passive CDEF for the exact changed pixels between two states.
pub const CHANGE_BOUNDS_DEFINITION: i16 = 6;
/// Full-precision passive progress state. Scalar button/scroll values are not
/// used as a lossy intermediate representation of file-byte counters.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProgressRange
{
    pub completed:   u64,
    pub in_progress: u64,
    pub total:       u64,
    /// Zero is an unknown denominator (all counts zero); one is determinate.
    pub determinate: u32,
    pub reserved:    u32
}
#[repr(C)]
pub struct ControlChange
{
    pub before: crate::definitions::ControlDefinitionState,
    pub after:  crate::definitions::ControlDefinitionState,
    /// Empty means the new state produces the same pixels.
    pub damage: [Rect; 2]
}
pub const BUTTON_PART: i16 = 10;
pub const CHECKBOX_PART: i16 = 11;
pub const SCROLL_UP: i16 = 20;
pub const SCROLL_DOWN: i16 = 21;
pub const SCROLL_PAGE_UP: i16 = 22;
pub const SCROLL_PAGE_DOWN: i16 = 23;
pub const SCROLL_THUMB: i16 = 129;
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollbarState
{
    pub bounds:      Rect,
    pub value:       i16,
    pub maximum:     i16,
    pub active:      u32,
    pub highlighted: i16,
    pub reserved:    i16
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ControlCall
{
    pub id:        u64,
    /// Native creation choice: 0 = no reflow, 1 = reflow. Read back by INFO_OBJECT.
    /// Non-creation requests must leave this zero; configuration cannot change it.
    pub reflow:    u32,
    pub layout:    reflow::Payload,
    pub flags:     u32,
    pub ticks:     u64,
    pub step:      i16,
    pub page:      i16,
    pub operation: u32,
    pub window:    u64,
    pub scrollbar: ScrollbarState,
    /// Hit-test point, in window-local coordinates. Drawing ignores it.
    pub point:     crate::Point,
    pub anchor:    crate::Point,
    pub result:    i32,
    pub standard:  crate::definitions::ControlDefinitionState,
    pub procedure: i16,
    pub reserved:  i16,
    /// Optional exclusive borrowed offscreen pixels, set only by safe transport.
    pub surface:   *mut crate::Surface
}
