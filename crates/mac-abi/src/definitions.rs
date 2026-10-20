//! Native definition message/state layouts shared by managers and code resources.
use crate::Rect;
pub const WDEF: u32 = u32::from_be_bytes(*b"WDEF");
pub const DRAW: i16 = 0;
pub const HIT: i16 = 1;
pub const CALCULATE: i16 = 2;
pub const NEW: i16 = 3;
pub const DISPOSE: i16 = 4;
pub const GROW: i16 = 5;
pub const DRAW_GROW_ICON: i16 = 6;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WindowDefinitionState
{
    pub content:               Rect,
    pub structure:             Rect,
    pub close_box:             Rect,
    pub grow_box:              Rect,
    pub active:                u32,
    pub close_pressed:         u32,
    pub title:                 [u8; crate::WINDOW_TITLE_BYTES],
    pub title_length:          u32,
    pub zoomed:                u32,
    pub zoom_box:              Rect,
    pub zoom_pressed:          u32,
    pub structure_piece_count: u32,
    pub structure_pieces:      [Rect; 2]
}

impl WindowDefinitionState
{
    pub fn new(content: Rect, title: &str) -> Self
    {
        let mut state = Self { content,
                               structure: Rect::ZERO,
                               close_box: Rect::ZERO,
                               grow_box: Rect::ZERO,
                               active: 0,
                               close_pressed: 0,
                               title: [0; crate::WINDOW_TITLE_BYTES],
                               title_length: 0,
                               zoomed: 0,
                               zoom_box: Rect::ZERO,
                               zoom_pressed: 0,
                               structure_piece_count: 0,
                               structure_pieces: [Rect::ZERO; 2] };
        let mut length = title.len().min(state.title.len());
        while !title.is_char_boundary(length)
        {
            length -= 1;
        }
        state.title[..length].copy_from_slice(&title.as_bytes()[..length]);
        state.title_length = length as u32;
        state
    }
}

pub const CDEF: u32 = u32::from_be_bytes(*b"CDEF");
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ControlDefinitionState
{
    pub bounds:       Rect,
    pub title:        [u8; crate::INLINE_STRING_BYTES],
    pub title_length: u32,
    pub enabled:      u32,
    pub highlighted:  u32,
    pub value:        i16,
    pub minimum:      i16,
    pub maximum:      i16,
    pub reserved:     i16,
    pub font:         crate::quickdraw::FontSelection
}
impl Default for ControlDefinitionState
{
    fn default() -> Self
    {
        Self { bounds:       Rect::ZERO,
               title:        [0; crate::WINDOW_TITLE_BYTES],
               title_length: 0,
               enabled:      1,
               highlighted:  0,
               value:        0,
               minimum:      0,
               maximum:      1,
               reserved:     0,
               font:         Default::default() }
    }
}
impl ControlDefinitionState
{
    pub fn new(bounds: Rect, title: &str) -> Self
    {
        let mut state = Self { bounds,
                               ..Self::default() };
        let mut n = title.len().min(state.title.len());
        while !title.is_char_boundary(n)
        {
            n -= 1;
        }
        state.title[..n].copy_from_slice(&title.as_bytes()[..n]);
        state.title_length = n as u32;
        state
    }
}
