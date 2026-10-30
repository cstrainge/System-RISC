//! Native Control Manager requests. CDEF resources own control geometry and pixels.
pub mod reflow;
use crate::Rect;
pub const DRAW_SCROLLBAR: u32 = 0;
pub const HIT_SCROLLBAR: u32 = 1;
pub const TRACK_SCROLLBAR: u32 = 2;
pub const TRACK_SPLITTER: u32 = 5;
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
/// Resident tree cursor and reverse-paint-order hit query. Never sent to CDEFs.
pub const NEXT_CHILD_OBJECT: u32 = 20;
pub const HIT_CHILDREN_OBJECT: u32 = 21;
/// Atomic Table appearance update; standard.flags contains TABLE_HIDE_* bits.
pub const SET_TABLE_LINES_OBJECT: u32 = 22;
/// Atomic selected-property update of a static-text object. Unselected fields,
/// including its current layout rectangle, remain resident-owned.
pub const SET_STATIC_TEXT_OBJECT: u32 = 23;
pub const TEXT_CONTENT: u32 = 1;
pub const TEXT_FONT: u32 = 2;
pub const TEXT_ALIGNMENT: u32 = 4;
pub const TEXT_LAYOUT: u32 = 8;
pub const TEXT_BOUNDS: u32 = 16;
pub const TEXT_PROPERTIES: u32 =
    TEXT_CONTENT | TEXT_FONT | TEXT_ALIGNMENT | TEXT_LAYOUT | TEXT_BOUNDS;
pub const TEXT_WRAP: i16 = 1;
/// Zero retains visible row/column separators and the outer Table border.
pub const TABLE_HIDE_ROWS: i16 = 1;
pub const TABLE_HIDE_COLUMNS: i16 = 2;
pub const TABLE_HIDE_BORDER: i16 = 4;
pub const TABLE_LINE_FLAGS: i16 = TABLE_HIDE_ROWS | TABLE_HIDE_COLUMNS | TABLE_HIDE_BORDER;
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
pub const TABLE_PROCEDURE: i16 = i16::MIN + 3;
pub const CDEF_PROGRESS_BAR: i16 = -32760;
pub const CDEF_STATIC_TEXT: i16 = -32759;
pub const CDEF_TABLE: i16 = -32758;
pub const SPLITTER_PROCEDURE: i16 = i16::MIN + 4;
pub const CDEF_SPLITTER: i16 = -32757;
pub const SPLITTER_PART: i16 = 130;
pub const SPLITTER_HORIZONTAL: i16 = 1;
pub const SPLITTER_RANGE: i16 = 2;
pub const SPLITTER_PARENT: i16 = 4;

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
    /// Resident relationship snapshot; output only except for child iteration.
    pub relation:  reflow::Relation,
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
