//! QuickDraw wire values. Drawing algorithms and font resources belong to System.
use crate::{Point, Rect, Surface};

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontSelection
{
    pub family: i16,
    pub size:   i16,
    pub face:   u16
}
impl Default for FontSelection
{
    fn default() -> Self
    {
        Self { family: 0,
               size:   12,
               face:   0 }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FontMetrics
{
    pub ascent:    i16,
    pub descent:   i16,
    pub leading:   i16,
    pub max_width: i16
}
pub const BOLD: u16 = 1;
pub const ITALIC: u16 = 2;
pub const UNDERLINE: u16 = 4;
pub const OUTLINE: u16 = 8;
pub const SHADOW: u16 = 16;
pub const CONDENSE: u16 = 32;
pub const EXTEND: u16 = 64;

/// The value part of a native port; clips with nonrectangular geometry are supplied
/// separately as classic region data. Coordinates are local to `origin`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PortState
{
    pub location:           Point,
    pub pen_size:           Point,
    pub mode:               i16,
    pub visibility:         i16,
    pub pattern:            [u8; 8],
    pub background_pattern: [u8; 8],
    pub foreground:         u32,
    pub background:         u32,
    pub clip:               Rect,
    pub origin:             Point
}
impl Default for PortState
{
    fn default() -> Self
    {
        Self { location:           Point::default(),
               pen_size:           Point { h: 1,
                                           v: 1 },
               mode:               8,
               visibility:         0,
               pattern:            [255; 8],
               background_pattern: [0; 8],
               foreground:         0,
               background:         0xffffff,
               clip:               Rect::new(i16::MIN, i16::MIN, i16::MAX, i16::MAX),
               origin:             Point::default() }
    }
}

/// Reserved onscreen desktop target; zero keeps its existing window semantics.
/// Offscreen world handles have the high bit set and cannot collide with this ID.
pub const DESKTOP_TARGET: u32 = 0x7fff_ffff;
/// Atomically replace the retained desktop scene through the regular QuickDraw gate.
pub const QD_DESKTOP_BEGIN: u32 = 32;
pub const QD_DESKTOP_END: u32 = 33;
pub const QD_DESKTOP_CANCEL: u32 = 34;
pub const QD_DESKTOP_BOUNDS: u32 = 35;

pub const QD_RESET: u32 = 0;
pub const QD_MOVE_TO: u32 = 1;
pub const QD_MOVE: u32 = 2;
pub const QD_LINE_TO: u32 = 3;
pub const QD_LINE: u32 = 4;
pub const QD_PEN_SIZE: u32 = 5;
pub const QD_PEN_MODE: u32 = 6;
pub const QD_PEN_PATTERN: u32 = 7;
pub const QD_BACK_PATTERN: u32 = 8;
pub const QD_COLORS: u32 = 9;
pub const QD_CLIP_RECT: u32 = 10;
pub const QD_ORIGIN: u32 = 11;
pub const QD_PEN_VISIBILITY: u32 = 12;
pub const QD_GET_STATE: u32 = 13;
pub const QD_SET_STATE: u32 = 14;
pub const QD_SHAPE: u32 = 16;
pub const QD_CLIP_REGION: u32 = 17;

pub const SHAPE_RECT: u32 = 0;
pub const SHAPE_OVAL: u32 = 1;
pub const SHAPE_ROUND_RECT: u32 = 2;
pub const SHAPE_ARC: u32 = 3;
pub const SHAPE_POLYGON: u32 = 4;
pub const SHAPE_REGION: u32 = 5;
pub const VERB_FRAME: u32 = 0;
pub const VERB_PAINT: u32 = 1;
pub const VERB_ERASE: u32 = 2;
pub const VERB_INVERT: u32 = 3;
pub const VERB_FILL: u32 = 4;

#[repr(C)]
#[derive(Default)]
pub struct GraphicsCall
{
    pub operation:     u32,
    pub window:        u32,
    pub surface:       *mut Surface,
    pub state:         PortState,
    pub shape:         u32,
    pub verb:          u32,
    pub bounds:        Rect,
    pub point:         Point,
    pub corner:        Point,
    pub start_angle:   i16,
    pub arc_angle:     i16,
    pub points:        *const Point,
    pub point_count:   usize,
    pub region:        *const u8,
    pub region_length: usize
}

pub const FONT_COUNT: u32 = 0;
pub const FONT_AT: u32 = 1;
pub const FONT_NAME: u32 = 2;
pub const FONT_ID: u32 = 3;
pub const FONT_METRICS: u32 = 4;
pub const FONT_WIDTH: u32 = 5;
pub const FONT_DRAW: u32 = 6;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FontCall
{
    pub operation:   u32,
    pub window:      u32,
    pub surface:     *mut Surface,
    pub selection:   FontSelection,
    pub text:        *const u8,
    pub text_length: usize,
    pub baseline:    Point,
    pub color:       u32,
    pub index:       u32,
    pub name:        [u8; crate::INLINE_STRING_BYTES],
    pub name_length: u32,
    pub width:       i16,
    pub metrics:     FontMetrics
}

pub const REGION_RECT: u32 = 0;
pub const REGION_POLYGON: u32 = 1;
pub const REGION_OVAL: u32 = 2;
pub const REGION_ROUND_RECT: u32 = 3;
pub const REGION_INTERSECT: u32 = 4;
pub const REGION_UNION: u32 = 5;
pub const REGION_DIFFERENCE: u32 = 6;
pub const REGION_XOR: u32 = 7;
pub const REGION_OFFSET: u32 = 8;
pub const REGION_INSET: u32 = 9;
pub const REGION_CONTAINS: u32 = 10;
pub const REGION_INTERSECTS: u32 = 11;
#[repr(C)]
#[derive(Default)]
pub struct RegionCall
{
    pub operation:   u32,
    pub a:           *const u8,
    pub a_length:    usize,
    pub b:           *const u8,
    pub b_length:    usize,
    pub bounds:      Rect,
    pub point:       Point,
    pub points:      *const Point,
    pub point_count: usize,
    pub output:      *mut u8,
    pub capacity:    usize,
    pub required:    usize,
    pub result:      u32
}

impl Default for FontCall
{
    fn default() -> Self
    {
        Self { operation:   0,
               window:      0,
               surface:     core::ptr::null_mut(),
               selection:   FontSelection::default(),
               text:        core::ptr::null(),
               text_length: 0,
               baseline:    Point::default(),
               color:       0,
               index:       0,
               name:        [0; crate::INLINE_STRING_BYTES],
               name_length: 0,
               width:       0,
               metrics:     FontMetrics::default() }
    }
}

pub const BITMAP_MONO: u32 = 1;
pub const BITMAP_RGB32: u32 = 32;
#[repr(C)]
#[derive(Default)]
pub struct BitmapCall
{
    pub window:           u32,
    pub surface:          *mut Surface,
    pub format:           u32,
    pub row_bytes:        usize,
    pub bounds:           Rect,
    pub source_rect:      Rect,
    pub destination_rect: Rect,
    pub data:             *const u8,
    pub data_length:      usize,
    pub palette:          *const u32,
    pub palette_length:   usize,
    pub mask:             *const u8,
    pub mask_length:      usize,
    pub mode:             i16,
    pub reserved:         u16,
    pub foreground:       u32,
    pub background:       u32
}

pub const WORLD_NEW: u32 = 0;
pub const WORLD_DISPOSE: u32 = 1;
pub const WORLD_INFO: u32 = 2;
pub const WORLD_READ: u32 = 3;
pub const WORLD_WRITE: u32 = 4;
pub const WORLD_COPY: u32 = 5;
#[repr(C)]
#[derive(Default)]
pub struct WorldCall
{
    pub operation:    u32,
    pub world:        u32,
    pub target:       u32,
    pub dimensions:   Point,
    pub source:       Rect,
    pub destination:  Rect,
    pub mode:         i16,
    pub reserved:     u16,
    pub pixels:       *mut u32,
    pub pixel_length: usize
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MonoCursor
{
    pub image:   [u16; 16],
    pub mask:    [u16; 16],
    pub hotspot: Point
}
pub const CURSOR_SET: u32 = 0;
pub const CURSOR_INIT: u32 = 1;
pub const CURSOR_HIDE: u32 = 2;
pub const CURSOR_SHOW: u32 = 3;
pub const CURSOR_OBSCURE: u32 = 4;
pub const CURSOR_SHIELD: u32 = 5;
pub const CURSOR_STATE: u32 = 6;
#[repr(C)]
#[derive(Default)]
pub struct CursorCall
{
    pub operation:   u32,
    pub cursor:      MonoCursor,
    pub bounds:      Rect,
    pub offset:      Point,
    pub colors:      *const u32,
    pub color_count: usize,
    pub visibility:  i32
}

pub const MAP_POINT: u32 = 0;
pub const MAP_RECTANGLE: u32 = 1;
pub const MAP_POLYGON: u32 = 2;
pub const SCALE_POINT: u32 = 3;
#[repr(C)]
#[derive(Default)]
pub struct MappingCall
{
    pub operation:   u32,
    pub source:      Rect,
    pub destination: Rect,
    pub point:       Point,
    pub rect:        Rect,
    pub points:      *mut Point,
    pub point_count: usize
}

pub const PICTURE_BYTES: u32 = 0;
pub const PICTURE_RESOURCE: u32 = 1;
pub const PICTURE_INFO: u32 = 2;
#[repr(C)]
#[derive(Default)]
pub struct PictureCall
{
    pub operation:     u32,
    pub window:        u32,
    pub surface:       *mut Surface,
    pub scope:         i16,
    pub id:            i16,
    pub data:          *const u8,
    pub length:        usize,
    pub destination:   Rect,
    pub bounds:        Rect,
    pub command_count: u32
}
pub const ICON_PLOT: u32 = 0;
pub const ICON_COUNT: u32 = 1;
pub const ICON_AT: u32 = 2;
pub const ICON_FILE: u32 = 3;
#[repr(C)]
#[derive(Default)]
pub struct IconCall
{
    pub operation:   u32,
    pub window:      u32,
    pub surface:     *mut Surface,
    pub scope:       i16,
    pub id:          i16,
    pub destination: Rect,
    pub flags:       u16,
    pub origin:      i16,
    pub index:       u32,
    pub creator:     [u8; 4],
    pub file_type:   [u8; 4]
}
