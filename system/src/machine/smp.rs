//! Normal boot CPU initialization. Real hardware admission precedes policy:
//! peers reach their canonical System idle and acknowledge a real startup SSIP,
//! while ordinary scheduling remains on the primary. No diagnostic substitute
//! or reduced-CPU fallback participates in this path.
#![forbid(unsafe_code)]
use alloc::{collections::TryReserveError, vec::Vec};
use core::{fmt,
           sync::atomic::{AtomicBool, Ordering}};

use super::{monitor::{self, PendingHart, ReadyHart, StartRefusal},
            traps::{self, EntryError}};

// The exact admitted owners live until reset. A failed boot instead returns
// all completed/current owners through StartError into the nonreturning boot
// refusal boundary; it must never retry over partially released hardware.
struct ReadySet
{
    harts: Vec<ReadyHart>
}
static READY: spin::Once<ReadySet> = spin::Once::new();
static ATTEMPTED: AtomicBool = AtomicBool::new(false);

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
        completed: Vec<ReadyHart>
    },
    Start
    {
        refusal:   StartRefusal,
        completed: Vec<ReadyHart>
    },
    Pending
    {
        cause:     monitor::Error,
        pending:   PendingHart,
        completed: Vec<ReadyHart>
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
                                      || -> Result<Vec<ReadyHart>, Error> {
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
        completed.push(ready);
    }
    // ATTEMPTED admits exactly one initializer. No other caller can publish a
    // competing set while this reset-lifetime owner is being installed.
    let ready = READY.call_once(|| ReadySet { harts: completed });
    serial!("SYSTEM_CPUS_READY primary={} admitted_peers={} dispatch=primary-only",
            current.number(),
            ready.harts.len());
    Ok(())
}
