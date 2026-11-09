//! Input is serviced before background maintenance at each scheduling boundary.
#![forbid(unsafe_code)]
use core::sync::atomic::{AtomicBool, Ordering};

/// One input-only turn gives interactive work an immediate return to its app.
/// A continuous input stream may defer maintenance only on alternate turns,
/// so I/O completion and cleanup cannot be starved by pointer traffic.
pub(crate) struct InputPriority
{
    deferred: AtomicBool
}
impl InputPriority
{
    pub(crate) const fn new() -> Self
    {
        Self { deferred: AtomicBool::new(false) }
    }

    pub(crate) fn background_allowed(&self, fresh_input: bool) -> bool
    {
        if fresh_input
        {
            self.deferred.fetch_xor(true, Ordering::Relaxed)
        }
        else
        {
            if self.deferred.load(Ordering::Relaxed)
            {
                self.deferred.store(false, Ordering::Relaxed);
            }
            true
        }
    }
}

