//! Native resource-based Alert subset. Item indices retain classic one-based IDs.
use crate::{Event, INLINE_STRING_BYTES};
pub const OPEN_ALERT: u32 = 0;
pub const HANDLE_EVENT: u32 = 1;
pub const CLOSE: u32 = 2;
pub const VALIDATE_ALERT: u32 = 4;
pub const DRAW: u32 = 3;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DialogCall
{
    pub operation:   u32,
    pub scope:       i16,
    pub resource:    i16,
    pub window:      u64,
    pub event:       Event,
    pub item:        u16,
    pub reserved:    u16,
    pub text_length: u32,
    /// UTF8 substitution for ^0 in the DITL, consumed by OPEN_ALERT/VALIDATE_ALERT.
    pub text:        [u8; INLINE_STRING_BYTES]
}
impl Default for DialogCall
{
    fn default() -> Self
    {
        Self { operation:   0,
               scope:       0,
               resource:    0,
               window:      0,
               event:       Event::default(),
               item:        0,
               reserved:    0,
               text_length: 0,
               text:        [0; INLINE_STRING_BYTES] }
    }
}
