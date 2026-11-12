//! The contract a driver publishes to be told its device interrupted.
//!
//! A driver that wants its device's interrupt lists this class in its module
//! declaration and takes the line from the device service (`INTERRUPT` on its
//! claim). From then on, when the device raises the line, System masks that
//! one line at the machine's interrupt controller and, from an ordinary turn
//! of the scheduler, calls SERVICE here with the line. No driver code runs
//! inside the trap.
//!
//! SERVICE is one bounded step: quiet the device - read and clear whatever
//! status holds its line up - do what that completes, and return. It must
//! complete in the call; it cannot answer Pending, because the line stays
//! masked until it returns and System unmasks it then. There is nothing to
//! acknowledge and so nothing to forget.
//!
//! A device whose line is level-triggered interrupts again at once if the
//! driver returns without quieting it. System counts that, and a line that
//! keeps firing faster than any device would is left masked and reported.
//!
//! The request is the line, four bytes little-endian: the number the device
//! service returned when the line was taken. A driver with one device may
//! ignore it. The reply is empty.
#![no_std]
#![forbid(unsafe_code)]

use mac_module::ClassId;

pub const CLASS: ClassId = ClassId(*b"INTR");
pub const VERSION: u32 = 1;
pub const SERVICE: u32 = 1;
pub const REQUEST_BYTES: usize = 4;
/// Carried in place of a line when SERVICE is the driver's alarm falling due
/// (the device service's `ALARM`) and not its device interrupting. No line was
/// masked for it and none is unmasked after.
pub const ALARM: u32 = u32::MAX;

/// The line a SERVICE request names.
pub fn line(request: &[u8]) -> Option<u32>
{
    Some(u32::from_le_bytes(request.try_into().ok()?))
}
