//! Native virtualized row viewport, composed with the standard CDEF scrollbar.
use crate::{Point, Rect, Surface, quickdraw::FontSelection};
pub const CREATE: u32 = 0;
pub const DISPOSE: u32 = 1;
pub const CONFIGURE: u32 = 2;
pub const INFO: u32 = 3;
pub const INVALIDATE: u32 = 4;
pub const SCROLL_TO: u32 = 5;
pub const SET_ROW: u32 = 6;
pub const DRAW: u32 = 7;
pub const TRACK_BEGIN: u32 = 8;
pub const TRACK_POLL: u32 = 9;
/// Resolve a complete, populated row under a local point; u32::MAX means none.
pub const ROW_AT: u32 = 10;
/// Release capture without applying a final scroll or requiring row rendering.
pub const TRACK_CANCEL: u32 = 11;
pub const SELECTED: u32 = 1;
pub const BUTTON_DOWN: u32 = 2;
pub const TRACKING: u32 = 4;
pub const CHANGED: u32 = 8;
pub const TEXT_BYTES: usize = 768;
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spec
{
    pub viewport:            Rect,
    pub scrollbar:           Rect,
    pub font:                FontSelection,
    pub padding:             Point,
    pub row_height:          i16,
    pub reserved:            u16,
    pub count:               u32,
    pub foreground:          u32,
    pub background:          u32,
    pub selected_foreground: u32,
    pub selected_background: u32
}
impl Default for Spec
{
    fn default() -> Self
    {
        Self { viewport:            Rect::ZERO,
               scrollbar:           Rect::ZERO,
               font:                FontSelection::default(),
               padding:             Point { h: 5,
                                            v: 12 },
               row_height:          16,
               reserved:            0,
               count:               0,
               foreground:          0,
               background:          0xffffff,
               selected_foreground: 0xffffff,
               selected_background: 0 }
    }
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Call
{
    pub operation:   u32,
    pub id:          u64,
    pub window:      u64,
    pub flags:       u32,
    pub spec:        Spec,
    pub top:         u32,
    pub first:       u32,
    pub end:         u32,
    pub row:         u32,
    pub point:       Point,
    pub ticks:       u64,
    pub part:        i16,
    pub reserved:    [u16; 3],
    pub text_length: u32,
    pub text:        [u8; TEXT_BYTES],
    pub surface:     *mut Surface
}
impl Default for Call
{
    fn default() -> Self
    {
        Self { operation:   INFO,
               id:          0,
               window:      0,
               flags:       0,
               spec:        Spec::default(),
               top:         0,
               first:       0,
               end:         0,
               row:         0,
               point:       Point::default(),
               ticks:       0,
               part:        0,
               reserved:    [0; 3],
               text_length: 0,
               text:        [0; TEXT_BYTES],
               surface:     core::ptr::null_mut() }
    }
}
