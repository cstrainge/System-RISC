//! Additive control layout vocabulary. These are definition message numbers,
//! not `ControlCall.operation` values or classic CDEF message translations.
//! Managed native instances opt in independently of classic variation bits.
//!
//! Bounds/visibility carry a Rect; single size setters carry Dimensions;
//! clear operations carry no payload. AskSize uses Constraints/Measurement,
//! and the paired settings operation uses SizePreferences. Relationship
//! payloads require the resident ownership transaction and family-specific slot
//! adapter; their numbers do not authorize raw-handle adoption through a CDEF.
use crate::Dimensions;

pub const ASK_SIZE: i16 = 30;
pub const ASSIGN_RECT: i16 = 31;
pub const INVALIDATE_LAYOUT: i16 = 32;
pub const SET_VISIBLE_RECT: i16 = 33;
pub const SET_CHILD: i16 = 34;
pub const SET_CHILD_INDEXED: i16 = 35;
pub const REMOVE_CHILD: i16 = 36;
pub const REPARENT: i16 = 37;
pub const SET_MINIMUM: i16 = 38;
pub const CLEAR_MINIMUM: i16 = 39;
pub const SET_PREFERRED: i16 = 40;
pub const CLEAR_PREFERRED: i16 = 41;
pub const SET_SIZE_PREFERENCES: i16 = 42;

pub const UNBOUNDED: u32 = 0;
pub const AT_MOST: u32 = 1;
pub const EXACT: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AxisConstraint
{
    pub kind:     u32,
    /// Nonnegative; must be zero for UNBOUNDED.
    pub extent:   i16,
    pub reserved: u16
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Constraints
{
    pub width:  AxisConstraint,
    pub height: AxisConstraint
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OptionalDimensions
{
    /// Exactly zero or one. Absent values must carry zero dimensions.
    pub present: u32,
    pub value:   Dimensions
}

/// Both fields replace the corresponding explicit override atomically. An
/// absent value clears the override; it never erases a calculated default.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SizePreferences
{
    pub minimum:   OptionalDimensions,
    pub preferred: OptionalDimensions
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Requirements
{
    pub minimum:   Dimensions,
    /// Effective preferred is at least minimum on each axis.
    pub preferred: Dimensions
}

/// Keep intrinsic needs separate from needs under the supplied constraints.
/// Neither minimum may be clamped down merely to conceal insufficient space.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Measurement
{
    pub intrinsic:   Requirements,
    pub constrained: Requirements
}

/// InvalidateLayout's u32 payload. Unknown bits and zero are malformed.
pub const INVALIDATE_MEASUREMENT: u32 = 1;
pub const INVALIDATE_ARRANGEMENT: u32 = 2;

// Explicit field layout on both host tooling and the native target. These
// assertions establish representation only, not target execution correctness.
const _: () = {
    assert!(core::mem::size_of::<Dimensions>() == 4);
    assert!(core::mem::offset_of!(Dimensions, height) == 2);
    assert!(core::mem::size_of::<AxisConstraint>() == 8);
    assert!(core::mem::size_of::<Constraints>() == 16);
    assert!(core::mem::size_of::<OptionalDimensions>() == 8);
    assert!(core::mem::size_of::<SizePreferences>() == 16);
    assert!(core::mem::size_of::<Requirements>() == 8);
    assert!(core::mem::size_of::<Measurement>() == 16);
};

/// Inline native request/reply storage. System lends this exclusively to the
/// CDEF during one synchronous call; it never contains caller-owned pointers.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Payload
{
    pub message:      i16,
    pub reserved:     u16,
    pub invalidation: u32,
    pub constraints:  Constraints,
    pub rect:         crate::Rect,
    pub size:         Dimensions,
    pub preferences:  SizePreferences,
    pub measurement:  Measurement,
    /// Resident-captured original size; callers cannot replace it.
    pub creation:     Dimensions,
    /// Relationship requests name existing generational Control Manager IDs.
    /// `ControlCall.id` is the current parent; REPARENT names its destination here.
    pub child:        u64,
    pub parent:       u64,
    pub cell:         Cell,
    pub span:         Span,
    pub anchors:      u32,
    pub track:        TrackSetting
}

/// A live tree cursor is invalidated by a membership change of its parent.
/// These are ordinary Control Manager identities, not a second handle registry.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Relation
{
    pub parent:   u64,
    pub revision: u64,
    pub child:    u64,
    pub cell:     Cell,
    pub span:     Span
}

/// Row/column placement in a sparse grid of actual child controls. No data rows
/// or renderer descriptors are implied; geometry is assigned separately.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cell
{
    pub row:    u32,
    pub column: u32
}

pub const SET_ANCHORS: i16 = 43;
pub const SET_GRID_TRACK: i16 = 44;
pub const GET_GRID_TRACK: i16 = 45;
/// Definition-only calculation. Never accepted from an application ControlCall.
pub const CALCULATE_GRID_AXIS: i16 = 46;
pub const ANCHOR_LEFT: u32 = 1;
pub const ANCHOR_RIGHT: u32 = 2;
pub const ANCHOR_TOP: u32 = 4;
pub const ANCHOR_BOTTOM: u32 = 8;
pub const TRACK_FIT: u32 = 0;
pub const TRACK_FILL: u32 = 1;
pub const TRACK_PERCENT: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span
{
    pub rows:    u32,
    pub columns: u32
}
impl Default for Span
{
    fn default() -> Self
    {
        Self { rows:    1,
               columns: 1 }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TrackSetting
{
    /// 0 = column, 1 = row.
    pub axis:    u32,
    pub index:   u32,
    pub mode:    u32,
    /// Hundredths of one percent; zero except in percent mode.
    pub percent: u32
}
/// Separate immutable inputs and mutable outputs are lent only by System.
/// Pointers never pass through the public Toolbox request transport.
#[repr(C)]
pub struct GridAxisCall
{
    pub constraint:  AxisConstraint,
    pub tracks:      *mut GridTrack,
    pub track_count: usize,
    pub items:       *mut GridItem,
    pub item_count:  usize,
    pub minimum:     i64,
    pub preferred:   i64
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GridTrack
{
    pub mode:      u32,
    pub percent:   u32,
    pub minimum:   i64,
    pub preferred: i64,
    pub assigned:  i64,
    pub position:  i64
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GridItem
{
    pub start:     u32,
    pub count:     u32,
    pub minimum:   i64,
    pub preferred: i64,
    /// Bit 0 = leading edge, bit 1 = trailing edge.
    pub anchors:   u32,
    pub reserved:  u32,
    pub position:  i64,
    pub extent:    i64
}

/// Definition-only borrowed outlines for the Table draw operation. System owns
/// the immutable backing and lends it only for the duration of painting.
#[repr(C)]
pub struct GridPaint
{
    pub cells: *const crate::Rect,
    pub count: usize
}
