//! Native Control Manager requests. CDEF resources own control geometry and pixels.
use crate::Rect;
pub const DRAW_SCROLLBAR: u32 = 0;
pub const HIT_SCROLLBAR: u32 = 1;
pub const TRACK_SCROLLBAR: u32 = 2;
pub const DRAW_STANDARD: u32 = 3;
pub const HIT_STANDARD: u32 = 4;
/// Native CDEF state validation before retained drawing; no surface required.
pub const VALIDATE_DEFINITION: i16 = 5;
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
    pub operation: u32,
    pub window:    u32,
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
