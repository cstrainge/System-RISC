//! Native definition message/state layouts shared by managers and code resources.
use crate::geometry::Rect;

pub const CALLBACK_NEXT: u32 = 0;
pub const CALLBACK_RETURN: u32 = 1;

/// The caller's identity comes from the canonical callback thread. NEXT borrows
/// an output buffer; RETURN borrows an input buffer. Neither accepts an owner,
/// thread, definition identity, completion ticket or supervisor return address.
#[repr(C)]
pub struct CallbackCall
{
    pub header:    crate::Header,
    pub operation: u32,
    pub reserved:  u32,
    pub buffer:    *mut u8,
    pub length:    usize,
    pub required:  usize,
    pub present:   u32,
    pub reserved2: u32,
    pub result:    i64
}
impl Default for CallbackCall
{
    fn default() -> Self
    {
        Self { header:    crate::Header::for_type::<Self>(),
               operation: CALLBACK_NEXT,
               reserved:  0,
               buffer:    core::ptr::null_mut(),
               length:    0,
               required:  0,
               present:   0,
               reserved2: 0,
               result:    0 }
    }
}

/// Byte offsets in the copied callback packet, never resident addresses.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct CallbackRange
{
    pub offset: u64,
    pub length: u64
}
impl CallbackRange
{
    pub fn checked(self, bytes: usize, alignment: usize) -> Option<core::ops::Range<usize>>
    {
        if alignment == 0 || !alignment.is_power_of_two()
        {
            return None;
        }
        let start = usize::try_from(self.offset).ok()?;
        let length = usize::try_from(self.length).ok()?;
        let end = start.checked_add(length)?;
        (start.is_multiple_of(alignment) && end <= bytes).then_some(start..end)
    }
}

pub const CALLBACK_SCALAR: u32 = 0;
pub const CALLBACK_BYTES: u32 = 1;
pub const CALLBACK_GRID_AXIS: u32 = 2;
pub const CALLBACK_GRID_PAINT: u32 = 3;
pub const CALLBACK_CONTROL_MESSAGE: u32 = 5;
/// Original user addresses consumed only by code in that same Process.
pub const CALLBACK_OPAQUE: u32 = 4;
pub const CALLBACK_SURFACE: u32 = 1;

/// Native DEFINITION transport. No executable gateway leaves System or CRT.
/// Addresses name user data, checked or kept opaque according to real source.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct UserCall
{
    pub header:    crate::Header,
    pub kind:      u32,
    pub id:        i16,
    pub variation: i16,
    pub message:   i16,
    pub reserved:  u16,
    pub reflow:    u32,
    pub param:     i64,
    pub object:    usize,
    pub surface:   usize,
    pub result:    i64
}

/// Kernel-produced work copied into an aligned user buffer. The CRT constructs
/// DefinitionCall and Surface locally. Return decoding uses the kernel's held
/// layout, not offsets, addresses, counts or entry values supplied by the user.
///
/// A manager-provided drawing surface is a bounded scratch viewport, not the
/// parent framebuffer. Its dimensions describe this buffer; origin preserves
/// logical coordinates and clip admits only the target pixels. The viewport's
/// physical offset is a multiple of eight, preserving native pattern phase.
/// Pixels outside clip are initialized to zero and never copied back. Authors
/// must honor the clip. Opaque direct calls retain their caller-owned surface.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CallbackPacket
{
    pub header:         crate::Header,
    pub entry:          u64,
    pub kind:           u32,
    pub id:             i16,
    pub variation:      i16,
    pub message:        i16,
    pub reserved:       u16,
    pub reflow:         u32,
    pub param:          i64,
    pub argument_kind:  u32,
    pub flags:          u32,
    pub object:         CallbackRange,
    pub arguments:      CallbackRange,
    pub auxiliary:      [CallbackRange; 2],
    pub properties:     CallbackRange,
    pub pixels:         CallbackRange,
    pub surface_width:  u32,
    pub surface_height: u32,
    pub surface_origin: crate::geometry::Point,
    pub surface_clip:   Rect,
    pub reserved2:      u32
}
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
/// Push-button bounds include its four-pixel default outline margin.
pub const DEFAULT_BUTTON: i16 = 1;
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ControlDefinitionState
{
    pub bounds:          Rect,
    pub title:           [u8; crate::INLINE_STRING_BYTES],
    pub title_length:    u32,
    pub enabled:         u32,
    pub highlighted:     u32,
    pub value:           i16,
    pub minimum:         i16,
    pub maximum:         i16,
    pub flags:           i16,
    pub font:            crate::quickdraw::FontSelection,
    pub progress:        crate::controls::ProgressRange,
    /// Optional parent geometry for a Splitter drag outline; SPLITTER_PARENT selects it.
    pub splitter_parent: Rect
}
impl Default for ControlDefinitionState
{
    fn default() -> Self
    {
        Self { bounds:          Rect::ZERO,
               title:           [0; crate::WINDOW_TITLE_BYTES],
               title_length:    0,
               enabled:         1,
               highlighted:     0,
               value:           0,
               minimum:         0,
               maximum:         1,
               flags:           0,
               font:            Default::default(),
               progress:        Default::default(),
               splitter_parent: Rect::ZERO }
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

/// Allocation-free definition fault report. The System supplies the current
/// invocation identity; the reporting code cannot select another definition.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DefinitionPanic
{
    pub length:  u32,
    pub flags:   u32,
    pub message: [u8; 1024]
}
impl DefinitionPanic
{
    pub const FORMAT_FAILED: u32 = 2;
    pub const TRUNCATED: u32 = 1;
}
impl Default for DefinitionPanic
{
    fn default() -> Self
    {
        Self { length:  0,
               flags:   0,
               message: [0; 1024] }
    }
}
