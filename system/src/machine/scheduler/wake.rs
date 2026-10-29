//! Coalesced primary work notification, separate from the authoritative queues.
//! Producers only publish an atomic hint; guard-free outer returns post MMIO.
//! Receiver observation is a genuine S interrupt ACK, never a successful store.
#![forbid(unsafe_code)]
use core::sync::atomic::{AtomicU8, Ordering};

use crate::machine::{monitor::{PrimaryWake, WakeError},
                     traps::execution::HartKey};

const PENDING: u8 = 1;
const POSTED: u8 = 2;
const RUNNABLE: u8 = 4;
const HINTS: u8 = PENDING | RUNNABLE;
static WORK: AtomicU8 = AtomicU8::new(0);
static RECEIVER: spin::Once<PrimaryWake> = spin::Once::new();
// Only the first failure publishes this payload. 1=publishing, 2=ready,
// 3=diagnosed. A newer unrelated failure cannot replace the original cause.
static FAILURE_STATE: AtomicU8 = AtomicU8::new(0);
static FAILURE: spin::Once<WakeError> = spin::Once::new();

/// Allocation-free, valid under an existing manager/cleanup guard. Before
/// receiver admission this retains real work without accessing COLD selection.
pub(crate) fn request()
{
    WORK.fetch_or(PENDING, Ordering::Release);
}

/// Pre-bootstrap Finder readiness is consumed by the initial direct start,
/// whereas a later real unblock must reach the primary receiver.
pub(super) fn runnable(primary_only: bool)
{
    if RECEIVER.get().is_some()
    {
        WORK.fetch_or(RUNNABLE, Ordering::Release);
    }
    if !primary_only
    {
        crate::machine::smp::runnable();
    }
}

/// A released claim exposes previously local captured work to other CPUs.
/// The releasing CPU already owns its fresh choice, so do not self-interrupt
/// it merely because its old thread joined a runnable queue.
pub(super) fn execution_released(hart: HartKey, primary_only: bool)
{
    if !hart.is_primary() && RECEIVER.get().is_some()
    {
        WORK.fetch_or(RUNNABLE, Ordering::Release);
    }
    if !primary_only
    {
        crate::machine::smp::execution_released(hart);
    }
}

/// The caller holds SCHEDULER while copying the complete primary policy Round
/// or making a fresh bootstrap choice. Every runnable edit uses that same guard.
/// A wake after that snapshot retains its hint even if the old policy decision
/// is still valid. Neither posted IRQ nor unrelated cleanup intent is ACKed here.
pub(super) fn observed_runnable()
{
    if WORK.load(Ordering::Relaxed) & RUNNABLE != 0
    {
        WORK.fetch_and(!RUNNABLE, Ordering::AcqRel);
    }
}

pub(super) fn admitted(receiver: PrimaryWake)
{
    RECEIVER.call_once(|| receiver);
    flush();
}

/// Caller is a masked, guard-free outer return or first-frame commitment.
/// POSTED coalesces producers until actual primary SACK; posting or refusal
/// clears no hint. Only actual queue observation may discharge RUNNABLE without
/// an IRQ. The real queues/tickets own the payload.
pub(crate) fn flush()
{
    crate::machine::smp::flush();
    let work = WORK.load(Ordering::Acquire);
    if work & HINTS == 0 || work & POSTED != 0
    {
        return;
    }
    let Some(receiver) = RECEIVER.get()
    else
    {
        return;
    };
    if WORK.try_update(Ordering::AcqRel, Ordering::Acquire, |work| {
               (work & HINTS != 0 && work & POSTED == 0).then_some(work | POSTED)
           })
           .is_err()
    {
        return;
    }
    if let Err(cause) = receiver.post()
    {
        if FAILURE_STATE.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_ok()
        {
            FAILURE.call_once(|| cause);
            FAILURE_STATE.store(2, Ordering::Release);
        }
        // POSTED and the general work hint stay retained. The actual primary
        // queue observation may discharge runnable work, but never fabricates
        // an IRQ ACK. These are binding/stop/exhaustion failures, not a reason
        // to spin retries or discard the original queues/tickets.
    }
}

pub(crate) fn acknowledged(hart: HartKey)
{
    crate::machine::smp::acknowledged(hart);
    if RECEIVER.get().is_some_and(|receiver| receiver.is_target(hart))
    {
        // Runs before this captured turn consumes queues. The Acquire side
        // observes producer publication. A producer after this clear leaves a
        // new pending hint for the outer return. Peer startup ACKs do nothing.
        WORK.swap(0, Ordering::AcqRel);
    }
}

pub(super) fn report_failure()
{
    if FAILURE_STATE.compare_exchange(2, 3, Ordering::AcqRel, Ordering::Acquire).is_ok()
    {
        if let Some(cause) = FAILURE.get()
        {
            serial!("PRIMARY_WAKE_RETAINED cause={cause}; original queued work and cleanup \
                     tickets retained");
        }
    }
}
