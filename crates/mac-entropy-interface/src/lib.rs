//! Device-neutral entropy provider contract. Only bytes actually supplied by
//! an admitted entropy source may complete FILL; zero bytes is not success.
//!
//! Operations have empty input. READY has no output and may defer while the
//! provider establishes its source. It completes only after a genuine source
//! completion, never from registration or a configured-device lookup alone.
//! FILL accepts 1..=MAX_TRANSFER bytes of output and may complete short with
//! 1..=capacity bytes. Callers must advance only by that actual count.
//!
//! Pending work belongs to its original broker ticket. Providers retain no
//! caller slice and acknowledge cancellation only after device ownership ends.
//! ERROR_TEXT is a bounded diagnostic snapshot, not a readiness/dependency
//! lease. READY also does not promise that a later fill cannot lose its source.
#![no_std]
#![forbid(unsafe_code)]

use mac_module::ClassId;

pub const CLASS: ClassId = ClassId(*b"ENTR");
pub const VERSION: u32 = 1;
pub const READY: u32 = 1;
pub const FILL: u32 = 2;
pub const ERROR_TEXT: u32 = 3;
pub const MAX_TRANSFER: usize = 2048;
pub const MAX_ERROR_TEXT: usize = 256;
