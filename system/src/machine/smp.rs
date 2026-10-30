//! Normal boot CPU initialization. Real hardware admission precedes policy:
//! peers reach their canonical System idle and acknowledge a real startup SSIP,
//! while ordinary scheduling remains on the primary. No diagnostic substitute
//! or reduced-CPU fallback participates in this path.
#![forbid(unsafe_code)]
use alloc::{collections::TryReserveError, vec::Vec};
use core::{fmt,
           sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering}};

use super::{monitor::{self, PendingHart, ReadyHart, StartRefusal},
            traps::{self, EntryError}};

// The exact admitted owners live until reset. A failed boot instead returns
// all completed/current owners through StartError into the nonreturning boot
// refusal boundary; it must never retry over partially released hardware.
struct ReadySet
{
    harts:      Vec<ReadyRow>,
    primary:    usize,
    generation: AtomicU64
}
impl ReadySet
{
    fn row(&self, hart: traps::execution::HartKey) -> Option<&ReadyRow>
    {
        let ordinal = hart.ordinal();
        if ordinal == self.primary
        {
            return None;
        }
        // Topology::checked requires strictly ordered hardware IDs. The
        // admission vector retains that exact order, omitting the primary.
        let slot = ordinal.checked_sub(usize::from(ordinal > self.primary))?;
        self.harts.get(slot).filter(|row| row.ready.binding().matches(hart))
    }
}
// Reset-lifetime readiness owners also retain their target-local wake state;
// neither hints nor CPU labels issue a second execution/admission authority.
pub(crate) struct ReadyRow
{
    ready:         ReadyHart,
    work:          AtomicU8,
    failure:       spin::Once<monitor::WakeError>,
    failure_state: AtomicU8
}
static READY: spin::Once<ReadySet> = spin::Once::new();
static ATTEMPTED: AtomicBool = AtomicBool::new(false);
const RUNNABLE: u8 = 1;
const POSTED: u8 = 2;
const DRAIN: u8 = 4;
static PEER_WORK: AtomicBool = AtomicBool::new(false);

pub(crate) fn active_harts() -> usize
{
    1 + READY.get().map_or(0, |set| {
                       set.harts
                          .iter()
                          .filter(|row| !traps::execution::dispatch::drained(&row.ready))
                          .count()
                   })
}

pub(crate) fn eligible_harts() -> usize
{
    1 + READY.get().map_or(0, |set| {
                       set.harts
                          .iter()
                          .filter(|row| traps::execution::dispatch::eligible(&row.ready))
                          .count()
                   })
}

/// Called under SCHEDULER and the same Active registration exclusion as the
/// actual frame claim. Every target is preflighted before any eligibility edit.
pub(crate) fn apply_harts(generation: u64, count: u32) -> Result<(), mac_abi::Status>
{
    let set = READY.get().ok_or(mac_abi::SERVICE_BUSY)?;
    if !traps::execution::selected_hart().is_primary()
       || generation == 0
       || count == 0
       || count as usize > set.harts.len() + 1
    {
        return Err(mac_abi::BAD_ARGUMENT);
    }
    let prior = set.generation.load(Ordering::Acquire);
    if prior == generation
       && set.harts.iter().enumerate().all(|(ordinal, row)| {
                                          if ordinal < count as usize - 1
                                          {
                                              traps::execution::dispatch::eligible(&row.ready)
                                          }
                                          else
                                          {
                                              traps::execution::dispatch::drained(&row.ready)
                                          }
                                      })
    {
        return Ok(());
    }
    if prior != 0
       && prior != generation
       && !set.harts.iter().all(|row| traps::execution::dispatch::drained(&row.ready))
    {
        return Err(mac_abi::SERVICE_BUSY);
    }
    for row in &set.harts
    {
        traps::execution::dispatch::check(&row.ready)?;
    }
    set.generation.store(generation, Ordering::Release);
    for (ordinal, row) in set.harts.iter().enumerate()
    {
        if ordinal < count as usize - 1
        {
            if !traps::execution::dispatch::eligible(&row.ready)
            {
                traps::execution::dispatch::admit(&row.ready);
                row.work.fetch_or(RUNNABLE, Ordering::Release);
                PEER_WORK.store(true, Ordering::Release);
            }
        }
        else if traps::execution::dispatch::eligible(&row.ready)
        {
            traps::execution::dispatch::withdraw(&row.ready);
            row.work.fetch_or(DRAIN, Ordering::Release);
            PEER_WORK.store(true, Ordering::Release);
        }
    }
    Ok(())
}

/// Withdrawal is not completion: a target still owns any running claim until
/// its own complete captured turn commits its affine idle frame.
pub(crate) fn withdraw_harts() -> bool
{
    let Some(set) = READY.get()
    else
    {
        return false;
    };
    for row in &set.harts
    {
        if traps::execution::dispatch::eligible(&row.ready)
        {
            traps::execution::dispatch::withdraw(&row.ready);
            row.work.fetch_or(DRAIN, Ordering::Release);
            PEER_WORK.store(true, Ordering::Release);
        }
    }
    set.harts.iter().all(|row| traps::execution::dispatch::drained(&row.ready))
}

/// New readiness is published by canonical THRD edits under SCHEDULER. It is
/// a per-change notification, not an interrupt or topology scan on mouse input.
pub(crate) fn runnable()
{
    runnable_except(None);
}

pub(crate) fn execution_released(hart: traps::execution::HartKey)
{
    runnable_except(Some(hart));
}

fn runnable_except(source: Option<traps::execution::HartKey>)
{
    if let Some(set) = READY.get()
    {
        for row in &set.harts
        {
            if traps::execution::dispatch::eligible(&row.ready)
               && source.is_none_or(|source| !row.ready.binding().matches(source))
            {
                row.work.fetch_or(RUNNABLE, Ordering::Release);
                PEER_WORK.store(true, Ordering::Release);
            }
        }
    }
}

pub(crate) fn observed_runnable(hart: traps::execution::HartKey)
{
    if let Some(row) = READY.get().and_then(|set| set.row(hart))
    {
        if row.work.load(Ordering::Relaxed) & RUNNABLE != 0
        {
            row.work.fetch_and(!RUNNABLE, Ordering::AcqRel);
        }
    }
}

pub(crate) fn acknowledged(hart: traps::execution::HartKey)
{
    if let Some(row) = READY.get().and_then(|set| set.row(hart))
    {
        row.work.swap(0, Ordering::AcqRel);
    }
}

/// Called only at guard-free masked outer return. A posted target's hint stays
/// retained until its actual SACK; all senders use the monitor's target epoch.
pub(crate) fn flush()
{
    if !PEER_WORK.swap(false, Ordering::AcqRel)
    {
        return;
    }
    let Some(set) = READY.get()
    else
    {
        return;
    };
    for row in &set.harts
    {
        if row.work
              .try_update(Ordering::AcqRel, Ordering::Acquire, |work| {
                  (work & (RUNNABLE | DRAIN) != 0 && work & POSTED == 0).then_some(work | POSTED)
              })
              .is_err()
        {
            continue;
        }
        if let Err(cause) = row.ready.post_work()
        {
            if row.failure_state.compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire).is_ok()
            {
                row.failure.call_once(|| cause);
                row.failure_state.store(2, Ordering::Release);
                super::scheduler::wake::request();
            }
            // Never retry-loop or invent ACK. The target hint and actual
            // queued thread/cleanup ownership remain retained.
        }
    }
}

pub(crate) fn report_failures()
{
    if let Some(set) = READY.get()
    {
        for row in &set.harts
        {
            if row.failure_state.compare_exchange(2, 3, Ordering::AcqRel, Ordering::Acquire).is_ok()
            {
                if let Some(cause) = row.failure.get()
                {
                    serial!("CPU_WAKE_RETAINED hart={} cause={cause}; queued work and cleanup \
                             retained",
                            row.ready.number());
                }
            }
        }
    }
}

pub(crate) enum Error
{
    ForeignPrimary,
    MissingTopology,
    TopologyBinding,
    AlreadyAttempted,
    Metadata
    {
        peers: usize,
        cause: TryReserveError
    },
    Prepare
    {
        hart:      u64,
        cause:     EntryError,
        completed: Vec<ReadyRow>
    },
    Start
    {
        refusal:   StartRefusal,
        completed: Vec<ReadyRow>
    },
    Pending
    {
        cause:     monitor::Error,
        pending:   PendingHart,
        completed: Vec<ReadyRow>
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::ForeignPrimary =>
            {
                out.write_str("CPU initialization requires the actual primary hart")
            }
            Self::MissingTopology =>
            {
                out.write_str("CPU initialization has no checked permanent topology")
            }
            Self::TopologyBinding =>
            {
                out.write_str("CPU initialization current row does not match its permanent \
                               topology ordinal")
            }
            Self::AlreadyAttempted =>
            {
                out.write_str("CPU initialization was already attempted; reset before retrying")
            }
            Self::Metadata { peers,
                             cause } =>
            {
                write!(out,
                       "CPU initialization could not reserve {peers} readiness owners before \
                        release: {cause}")
            }
            Self::Prepare { hart,
                            cause,
                            completed } =>
            {
                write!(out,
                       "CPU {hart} preparation refused after {} peer admissions: {cause}; \
                        completed owners retained",
                       completed.len())
            }
            Self::Start { refusal,
                          completed } =>
            {
                write!(out,
                       "CPU {} release refused before publication after {} peer admissions: {}; \
                        preparation and completed owners retained",
                       refusal.prepared.number(),
                       completed.len(),
                       refusal.cause)
            }
            Self::Pending { cause,
                            pending,
                            completed } =>
            {
                write!(out,
                       "CPU {} initialization failed after publication and {} peer admissions: \
                        {cause}; published claim and completed owners retained until reset",
                       pending.number(),
                       completed.len())
            }
        }
    }
}

pub(super) fn initialize() -> Result<(), Error>
{
    // The only caller runs on the primary's boot service stack, after canonical
    // idles exist and before the scheduler/first application handoff. This key
    // is authenticated from actual sscratch, not reconstructed from topology.
    let current = traps::execution::selected_hart();
    if super::paging::firmware_owner_hart() != Some(current.number())
    {
        return Err(Error::ForeignPrimary);
    }
    let topology = super::paging::enabled_cpus().ok_or(Error::MissingTopology)?;
    let primary = current.ordinal();
    if topology.is_empty()
       || topology.get(primary).is_none_or(|cpu| cpu.hart != current.number() as u64)
    {
        return Err(Error::TopologyBinding);
    }
    ATTEMPTED.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
             .map_err(|_| Error::AlreadyAttempted)?;
    let peers = topology.len() - 1;
    // This is real System metadata, not an application's account or a recovery
    // allocation. Reserve every result slot before the first physical release.
    let mut completed = crate::context::without_recovery(|| {
        crate::allocator::with_system(crate::allocator::SystemClass::Foundation,
                                      || -> Result<Vec<ReadyRow>, Error> {
                                          let mut owners = Vec::new();
                                          owners.try_reserve_exact(peers).map_err(|cause| Error::Metadata { peers, cause })?;
                                          Ok(owners)
                                      })
    })?;
    for (ordinal, cpu) in topology.iter().enumerate()
    {
        if ordinal == primary
        {
            continue;
        }
        let prepared = match traps::prepare_secondary(ordinal)
        {
            Ok(prepared) => prepared,
            Err(cause) =>
            {
                return Err(Error::Prepare { hart: cpu.hart,
                                            cause,
                                            completed });
            }
        };
        let mut pending = match monitor::begin_secondary(prepared)
        {
            Ok(pending) => pending,
            Err(refusal) =>
            {
                return Err(Error::Start { refusal,
                                          completed });
            }
        };
        let ready = loop
        {
            // into_ready polls the real monitor deadline using the checked DT
            // timebase and hardware rdtime. No host acknowledgment, scheduler
            // lock, frame borrow or UI dispatch serializes this transition.
            match pending.into_ready()
            {
                Ok(ready) => break ready,
                Err((monitor::Error::Busy, retained)) =>
                {
                    pending = retained;
                    core::hint::spin_loop();
                }
                Err((cause, pending)) =>
                {
                    return Err(Error::Pending { cause,
                                                pending,
                                                completed });
                }
            }
        };
        // Actual capacity covers exactly one result per discovered peer. No
        // ordinary allocation can occur after any physical release.
        serial!("SYSTEM_CPU_ADMITTED hart={} ordinal={} start_sequence={} wake_epoch={} \
                 dispatch=idle-only",
                ready.number(),
                ready.ordinal(),
                ready.sequence(),
                ready.wake_epoch());
        completed.push(ReadyRow { ready,
                                  work: AtomicU8::new(0),
                                  failure: spin::Once::new(),
                                  failure_state: AtomicU8::new(0) });
    }
    // ATTEMPTED admits exactly one initializer. No other caller can publish a
    // competing set while this reset-lifetime owner is being installed.
    let ready = READY.call_once(|| {
                         ReadySet { harts: completed,
                                    primary,
                                    generation: AtomicU64::new(0) }
                     });
    serial!("SYSTEM_CPUS_READY primary={} admitted_peers={} dispatch=primary-only",
            current.number(),
            ready.harts.len());
    Ok(())
}
