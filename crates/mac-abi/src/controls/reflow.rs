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
    pub creation:     Dimensions
}
