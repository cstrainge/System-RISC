//! Native Control Manager requests. CDEF resources own control geometry and pixels.
pub mod reflow;
use crate::geometry::Rect;
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

/// System CDEF resource IDs occupy 0..=127. Procedure IDs use the classic
/// encoding: resource ID in the upper bits, variation in the low four bits.
pub const CDEF_PROGRESS_BAR: i16 = 5;
pub const CDEF_STATIC_TEXT: i16 = 18;
pub const CDEF_TABLE: i16 = 27;
pub const CDEF_SPLITTER: i16 = 28;
pub const CDEF_SEPARATOR: i16 = 29;
pub const PROGRESS_BAR_PROCEDURE: i16 = CDEF_PROGRESS_BAR << 4;
pub const STATIC_TEXT_PROCEDURE: i16 = CDEF_STATIC_TEXT << 4;
pub const TABLE_PROCEDURE: i16 = CDEF_TABLE << 4;
pub const SPLITTER_PROCEDURE: i16 = CDEF_SPLITTER << 4;
pub const SEPARATOR_PROCEDURE: i16 = CDEF_SEPARATOR << 4;

/// Native progress presentation flag, independent of the classic procedure.
/// Procedure 81 belongs to the Appearance relevance bar, not monochrome mode.
pub const PROGRESS_MONOCHROME: i16 = 1;
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
/// A tab of a Tab control is its own part: this plus the tab's position in the
/// control's list. A press on one tracks, highlights and activates exactly as
/// a button does.
pub const TAB_PART: i16 = 1000;
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
    pub point:     crate::geometry::Point,
    pub anchor:    crate::geometry::Point,
    pub result:    i32,
    pub standard:  crate::definitions::ControlDefinitionState,
    pub procedure: i16,
    pub reserved:  i16,
    /// Optional exclusive borrowed offscreen pixels, set only by safe transport.
    pub surface:   *mut crate::Surface
}

/// V2 control property vocabulary. The decoder is shared with loaded CDEFs.
pub use mac_control_model as suite;
pub const CDEF_GROUP_BOX: i16 = 30;
pub const GROUP_BOX_PROCEDURE: i16 = CDEF_GROUP_BOX << 4;
pub const CDEF_PICTURE: i16 = 31;
pub const PICTURE_PROCEDURE: i16 = CDEF_PICTURE << 4;
pub const CDEF_ACTIVITY_INDICATOR: i16 = 32;
pub const ACTIVITY_INDICATOR_PROCEDURE: i16 = CDEF_ACTIVITY_INDICATOR << 4;
pub const CDEF_DROP_MENU: i16 = 25;
pub const DROP_MENU_PROCEDURE: i16 = CDEF_DROP_MENU << 4;
/// Classic fixed-width popup variation. Standard File is a real customer;
/// variation zero remains the suite's rectangle-and-X stub.
pub const DROP_MENU_FIXED_PROCEDURE: i16 = DROP_MENU_PROCEDURE | 1;
pub const CDEF_LIST_BOX: i16 = 33;
pub const LIST_BOX_PROCEDURE: i16 = CDEF_LIST_BOX << 4;
pub const CDEF_SEARCH_FIELD: i16 = 34;
pub const SEARCH_FIELD_PROCEDURE: i16 = CDEF_SEARCH_FIELD << 4;
pub const CDEF_OVERFLOW_ARROWS: i16 = 35;
pub const OVERFLOW_ARROWS_PROCEDURE: i16 = CDEF_OVERFLOW_ARROWS << 4;
pub const CDEF_SLIDER: i16 = 36;
pub const SLIDER_PROCEDURE: i16 = CDEF_SLIDER << 4;
pub const CDEF_STEPPER: i16 = 37;
pub const STEPPER_PROCEDURE: i16 = CDEF_STEPPER << 4;
pub const CDEF_DISCLOSURE_TRIANGLE: i16 = 38;
pub const DISCLOSURE_TRIANGLE_PROCEDURE: i16 = CDEF_DISCLOSURE_TRIANGLE << 4;
pub const CDEF_METER: i16 = 39;
pub const METER_PROCEDURE: i16 = CDEF_METER << 4;
pub const CDEF_SEGMENTED: i16 = 40;
pub const SEGMENTED_PROCEDURE: i16 = CDEF_SEGMENTED << 4;
pub const CDEF_DATE_TIME: i16 = 41;
pub const DATE_TIME_PROCEDURE: i16 = CDEF_DATE_TIME << 4;
pub const CDEF_LIST_EDIT: i16 = 42;
pub const LIST_EDIT_PROCEDURE: i16 = CDEF_LIST_EDIT << 4;
pub const CDEF_TAB: i16 = 43;
pub const TAB_PROCEDURE: i16 = CDEF_TAB << 4;
pub const CDEF_BREADCRUMB: i16 = 44;
pub const BREADCRUMB_PROCEDURE: i16 = CDEF_BREADCRUMB << 4;
pub const CDEF_COLOR_WELL: i16 = 45;
pub const COLOR_WELL_PROCEDURE: i16 = CDEF_COLOR_WELL << 4;
pub const CDEF_TREE: i16 = 46;
pub const TREE_PROCEDURE: i16 = CDEF_TREE << 4;
pub const CDEF_ICON_VIEW: i16 = 47;
pub const ICON_VIEW_PROCEDURE: i16 = CDEF_ICON_VIEW << 4;
pub const CDEF_GRAPH: i16 = 48;
pub const GRAPH_PROCEDURE: i16 = CDEF_GRAPH << 4;
pub const CDEF_TOOLTIP: i16 = 49;
pub const TOOLTIP_PROCEDURE: i16 = CDEF_TOOLTIP << 4;
pub const CDEF_TEXT_EDIT: i16 = 17;
pub const TEXT_EDIT_PROCEDURE: i16 = CDEF_TEXT_EDIT << 4;
pub const TEXT_EDIT_VIEW_PROCEDURE: i16 = TEXT_EDIT_PROCEDURE | 1;
pub const TEXT_EDIT_PASSWORD_PROCEDURE: i16 = TEXT_EDIT_PROCEDURE | 2;

/// Instance property calls share CONTROL and canonical CTRL identities. The
/// bytes use the shared typed model codec, never app pointers retained by CTRL.
pub const GET_PROPERTIES: u32 = 0;
pub const SET_PROPERTIES: u32 = 1;
/// Typed requests are delivered to the object's real retained definition.
pub const SEND_MESSAGE: u32 = 2;
pub const MESSAGE_DEFINITION: i16 = 7;
#[repr(C)]
pub struct MessageData
{
    pub data:   *const u8,
    pub length: usize
}
pub const ALL_PROPERTIES: u32 = u32::MAX;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PropertiesCall
{
    pub id:        u64,
    pub operation: u32,
    pub field:     u32,
    pub input:     *const u8,
    pub length:    usize,
    pub output:    *mut u8,
    pub capacity:  usize,
    pub required:  usize,
    pub revision:  u64
}
impl Default for PropertiesCall
{
    fn default() -> Self
    {
        Self { id:        0,
               operation: GET_PROPERTIES,
               field:     ALL_PROPERTIES,
               input:     core::ptr::null(),
               length:    0,
               output:    core::ptr::null_mut(),
               capacity:  0,
               required:  0,
               revision:  0 }
    }
}
