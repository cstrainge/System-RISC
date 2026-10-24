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

#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn a_new_input_turn_defers_background_and_a_quiet_turn_runs_it()
    {
        let priority = InputPriority::new();
        assert!(priority.background_allowed(false));
        assert!(!priority.background_allowed(true));
        assert!(priority.background_allowed(false));
        assert!(!priority.background_allowed(true));
    }

    #[test]
    fn continuous_input_cannot_starve_background_progress()
    {
        let priority = InputPriority::new();
        let mut progress = 0;
        for _ in 0..1000
        {
            assert!(!priority.background_allowed(true));
            assert!(priority.background_allowed(true));
            progress += 1;
        }
        assert_eq!(progress, 1000);
        assert!(priority.background_allowed(false));
        assert!(!priority.background_allowed(true));
    }
}
