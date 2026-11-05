//! Wall-clock source for a board that has none.
//!
//! This SoC carries a free-running system timer, which is monotonic and has no
//! relation to calendar time, and no battery-backed real-time clock. Reporting
//! seconds since boot as a `Timestamp` would present a wrong date as a real one,
//! so this returns the absence instead and lets each caller decide.
//!
//! That is the behaviour callers already implement: a file write that needs a
//! timestamp is refused rather than stamped with a fabricated date, and a
//! display that only wants a best-effort value falls back to zero.
//!
//! When something can supply an epoch - a firmware-passed value, a network time
//! source, or a driver module - it belongs here as a settable origin with the
//! system timer supplying elapsed time, so the clock becomes real without any
//! caller changing.

use mac_abi::{Status, dates::Timestamp};

/// Calendar time is unavailable on this platform.
pub(crate) fn now() -> Result<Timestamp, Status>
{
    Err(mac_abi::NOT_SUPPORTED)
}
