//! Reclamation follows actual stack/async-resource completion, never input.
//! This gate covers resident ledgers and saved state. File/provider lifetime
//! registration and their independent I/O barrier remain staged migration work.
#![forbid(unsafe_code)]
use core::sync::atomic::{AtomicBool, Ordering};

static CHANGED: AtomicBool = AtomicBool::new(false);

pub(crate) fn thread_reclaimed()
{
    super::finder::thread_reclaimed();
    CHANGED.store(true, Ordering::Release);
}

pub(crate) fn resources_released()
{
    CHANGED.store(true, Ordering::Release);
}

pub(crate) fn pump()
{
    if !CHANGED.load(Ordering::Acquire) || !CHANGED.swap(false, Ordering::AcqRel)
    {
        return;
    }
    if crate::kernel::with_desktop(|desktop| desktop.reap_processes()).is_err()
    {
        // A busy desktop has not consumed the checkpoint. Keep the request,
        // rather than losing the only path to retained manager state.
        CHANGED.store(true, Ordering::Release);
    }
}
