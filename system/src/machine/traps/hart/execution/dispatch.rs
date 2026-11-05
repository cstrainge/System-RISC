//! Dispatch admission and drain use real hardware-readiness owners. Callers
//! serialize phase edits with SCHEDULER and the active policy's commit turn.
#![forbid(unsafe_code)]
use core::sync::atomic::Ordering;

use mac_abi::{BAD_ABI, SERVICE_BUSY, Status};

use super::{HartKey, IDLE};
use crate::machine::{monitor::ReadyHart, traps::TrapFrame};

pub(crate) fn check(ready: &ReadyHart) -> Result<(), Status>
{
    let binding = ready.binding();
    let row = binding.0;
    if binding.is_primary()
       || ready.sequence() == 0
       || ready.wake_epoch() == 0
       || !matches!(row.phase.load(Ordering::Acquire),
                    super::super::ADMITTED_IDLE
                    | super::super::DISPATCH_ELIGIBLE
                    | super::super::DRAINING)
       || !super::idle_prepared(&row.local)
    {
        return Err(BAD_ABI);
    }
    let (_, stopped) = crate::machine::monitor::cpu_observation(binding.ordinal(),
                                                              binding.number() as u64)
        .ok_or(BAD_ABI)?;
    if stopped
    {
        return Err(SERVICE_BUSY);
    }
    Ok(())
}

/// Preflight of every selected receipt precedes all edits. No fallible step or
/// foreign callback follows an edit in the caller's serialized publication.
pub(crate) fn admit(ready: &ReadyHart)
{
    ready.binding().0.phase.store(super::super::DISPATCH_ELIGIBLE, Ordering::Release);
}

/// Only the target may finish a drain. The issuer withdraws eligibility and
/// posts work; it cannot forge an idle/released-frame acknowledgment remotely.
pub(crate) fn withdraw(ready: &ReadyHart)
{
    let row = ready.binding().0;
    if row.phase.load(Ordering::Acquire) == super::super::DISPATCH_ELIGIBLE
    {
        row.phase.store(super::super::DRAINING, Ordering::Release);
    }
}

pub(crate) fn eligible(ready: &ReadyHart) -> bool
{
    ready.binding().0.phase.load(Ordering::Acquire) == super::super::DISPATCH_ELIGIBLE
}

pub(crate) fn drained(ready: &ReadyHart) -> bool
{
    ready.binding().0.phase.load(Ordering::Acquire) == super::super::ADMITTED_IDLE
}

/// Called after actual idle selection and old THRD claim release, still in
/// the owning target's complete outer-capture turn under SCHEDULER. Verify the
/// real affine frame and active ContextRef before publishing the receipt.
pub(crate) fn idle_committed(hart: HartKey, frame: *mut TrapFrame)
{
    let row = hart.0;
    if row.phase.load(Ordering::Acquire) != super::super::DRAINING
    {
        return;
    }
    if hart != super::selected_hart()
       || super::current() != IDLE
       || !super::admitted_idle_capture(&row.local, frame)
    {
        super::super::super::entry_lost("CPU drain lacks its actual affine idle/frame commitment");
    }
    row.phase.store(super::super::ADMITTED_IDLE, Ordering::Release);
    // Actual ownership completion, rather than a monitor IRQ ACK, wakes the
    // primary's retirement walk. It retains every original policy/code owner.
    crate::drivers::scheduler::cleanup_progress();
    crate::machine::scheduler::wake::request();
}
