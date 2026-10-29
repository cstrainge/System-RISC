//! Replaceable scheduling policy consumes canonical THRD facts; the native
//! core retains frame/claim authority. Initial takeover remains primary-only.
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::{ACCESS_DENIED, BAD_ABI, NO_MEMORY, NOT_SUPPORTED, SERVICE_BUSY, Status};
use mac_scheduler_interface::{self as wire, control};
use spinning_top::Spinlock;

// The quantum and late-return allowance are independently checked platform
// choices. Neither is a latency guarantee or recovery from a nonreturning call.
use super::{CALLBACK_BUDGET_MICROSECONDS, QUANTUM_MICROSECONDS};
use super::{IDLE, SCHEDULER, Scheduler, validate_capture};
use crate::{machine::traps::OuterCapture, toolbox::handles::OwnerRef};

pub(crate) fn limits() -> Result<control::Limits, Status>
{
    let cpus = crate::machine::paging::enabled_cpus().ok_or(BAD_ABI)?;
    let boot_hart = crate::machine::paging::firmware_owner_hart().ok_or(BAD_ABI)? as u64;
    if !crate::machine::traps::execution::primary_dispatch_only()
    {
        return Err(SERVICE_BUSY);
    }
    let result =
        control::Limits { features: control::TAKEOVER,
                          maximum_candidates: wire::MAX_CANDIDATES as u32,
                          maximum_active_harts: 1,
                          minimum_quantum_microseconds: QUANTUM_MICROSECONDS,
                          maximum_quantum_microseconds: QUANTUM_MICROSECONDS,
                          callback_budget_microseconds: CALLBACK_BUDGET_MICROSECONDS,
                          physical_harts: cpus.len().try_into().map_err(|_| NOT_SUPPORTED)?,
                          active_harts: 1,
                          boot_hart,
                          // The admitted boot topology is immutable until reset. No hotplug
                          // producer exists; requests from a different epoch are refused.
                          topology_epoch: 1 };
    result.validate().map_err(|_| BAD_ABI)?;
    Ok(result)
}

pub(crate) struct Plan
{
    limits:        control::Limits,
    scratch:       Spinlock<Option<Scratch>>,
    late_reported: AtomicBool
}
struct Scratch
{
    records:  Vec<wire::Record>,
    packet:   Vec<u8>,
    sequence: u64
}
impl Plan
{
    /// Runtime has selected this actual module's Kernel account before entry.
    /// Nothing is published if either complete-set buffer cannot be admitted.
    pub(crate) fn prepare(owner: &OwnerRef, requirements: control::Requirements)
                          -> Result<Self, Status>
    {
        if owner.owner().kind() != mac_object_table::domain::DomainKind::Module
           || !owner.accepting()
        {
            return Err(ACCESS_DENIED);
        }
        let limits = limits()?;
        requirements.validate(limits).map_err(|_| NOT_SUPPORTED)?;
        let mut records = Vec::new();
        records.try_reserve_exact(wire::MAX_CANDIDATES).map_err(|_| NO_MEMORY)?;
        let mut packet = Vec::new();
        packet.try_reserve_exact(wire::MAX_ROUND_BYTES).map_err(|_| NO_MEMORY)?;
        packet.resize(wire::MAX_ROUND_BYTES, 0);
        Ok(Self { limits,
                  late_reported: AtomicBool::new(false),
                  scratch: Spinlock::new(Some(Scratch { records,
                                                        packet,
                                                        sequence: 0 })) })
    }

    pub(crate) fn limits(&self) -> control::Limits
    {
        self.limits
    }

    /// The adapter has already withdrawn new leases and bootstrap-selects.
    /// Ready peers still idle in S mode; none has received a dispatch grant.
    pub(crate) fn retire(&self) -> Result<bool, Status>
    {
        Ok(crate::machine::traps::execution::primary_dispatch_only()
           && self.scratch.lock().is_some())
    }

    fn scratch(&self) -> Result<ScratchLoan<'_>, Status>
    {
        let scratch = self.scratch.lock().take().ok_or(SERVICE_BUSY)?;
        Ok(ScratchLoan { plan:    self,
                         scratch: Some(scratch) })
    }
}

/// The only writer of an empty scratch slot is the loan that took its value.
/// No packet or scratch lock crosses module code. Refusal returns the same
/// admitted buffers for later retry or real disposal.
struct ScratchLoan<'a>
{
    plan:    &'a Plan,
    scratch: Option<Scratch>
}
impl Drop for ScratchLoan<'_>
{
    fn drop(&mut self)
    {
        *self.plan.scratch.lock() = self.scratch.take();
    }
}

/// Minted only after a real shadow decision survives live THRD revalidation.
/// The adapter cannot substitute a public State, boolean or numeric generation.
pub(crate) struct ActivationProof
{
    policy: crate::drivers::scheduler::PolicyPin
}
impl ActivationProof
{
    pub(crate) fn into_policy(self) -> crate::drivers::scheduler::PolicyPin
    {
        self.policy
    }
}

pub(super) struct Selection
{
    thread:                   Option<u64>,
    pub(super) quantum_ticks: u64
}
impl Selection
{
    /// Revalidate at the actual frame/claim commitment, after all foreign code
    /// and startup publication. Stale output never authorizes a reused slot.
    pub(super) fn slot(&self, scheduler: &Scheduler, capture: &OuterCapture) -> Option<usize>
    {
        match self.thread
        {
            Some(id) => scheduler.iter().find_map(|(slot, thread)| {
                (thread.id == id && eligible(slot, thread.state.get(), capture)
                 && (slot == capture.current() || thread.core.cpu_frame().is_some())).then_some(slot)
            }),
            None => (!scheduler.iter().any(|(slot, thread)| {
                eligible(slot, thread.state.get(), capture)
            })).then_some(IDLE)
        }
    }
}

fn eligible(slot: usize, state: super::RunState, capture: &OuterCapture) -> bool
{
    state.live
    && state.affinity.allows(capture.hart())
    && !state.retired
    && state.waiting.is_none()
    && (state.claimed_by.is_none()
        || (slot == capture.current() && state.claimed_by == Some(capture.hart())))
}

/// Complete copied facts and packet storage are prepaid by the registration.
/// The shipping bootstrap policy is still available after any typed refusal.
pub(super) struct Expired;
enum Rejection
{
    Expired,
    Permanent(Status, &'static str)
}
impl From<(Status, &'static str)> for Rejection
{
    fn from((status, detail): (Status, &'static str)) -> Self
    {
        Self::Permanent(status, detail)
    }
}
pub(super) fn select(capture: &OuterCapture) -> Result<Option<Selection>, Expired>
{
    use crate::drivers::scheduler::{self as adapter, Candidate};
    let (pin, shadow) = match adapter::candidate()
    {
        Ok(Some(Candidate::Shadow(pin))) => (pin, true),
        Ok(Some(Candidate::Active(pin))) => (pin, false),
        Ok(None) | Err(_) => return Ok(None)
    };
    let result = select_from(&pin, capture);
    let selection = match result
    {
        Ok(selection) => selection,
        Err(Rejection::Expired) => return Err(Expired),
        Err(Rejection::Permanent(status, detail)) =>
        {
            // Per registration change, never a per-input/mouse log. Refusal
            // revokes further callback leases and retains the real owner.
            serial!("SCHEDULER_POLICY_REFUSED generation={} status={} cause={}",
                    pin.generation(),
                    status,
                    detail);
            if let Err(retained) = adapter::refuse(&pin, status)
            {
                serial!("SCHEDULER_POLICY_RETIRE_RETAINED generation={} status={}",
                        pin.generation(),
                        retained);
            }
            return Ok(None);
        }
    };
    // No independent author runs between this revalidation and frame commit:
    // ordinary dispatch is primary-only and SIE remains masked throughout.
    // Enabling cross-hart authors requires a stronger joint commit contract.
    if shadow
    {
        if let Err(status) = adapter::commit_shadow(ActivationProof { policy: pin })
        {
            serial!("SCHEDULER_SHADOW_COMMIT_REFUSED status={status}; bootstrap retained");
            return Ok(None);
        }
    }
    else if !matches!(adapter::current(&pin), Ok(true))
    {
        return Ok(None);
    }
    Ok(Some(selection))
}

fn select_from(pin: &crate::drivers::scheduler::PolicyPin, capture: &OuterCapture)
               -> Result<Selection, Rejection>
{
    let lease = crate::drivers::scheduler::begin_call(pin).map_err(|status| {
                                                              (status,
                                                               "original policy registration \
                                                                cannot admit a callback")
                                                          })?;
    let plan = lease.plan();
    let mut loan =
        plan.scratch().map_err(|status| (status, "original policy scratch is already in use"))?;
    let scratch =
        loan.scratch.as_mut().ok_or((BAD_ABI, "policy scratch loan has no retained buffers"))?;
    scratch.records.clear();
    let now_microseconds = crate::machine::monitor::ticks_to_microseconds(crate::platform::ticks())
        .map_err(|_| (BAD_ABI, "admitted clock cannot represent this scheduler round"))?;
    let previous_thread;
    {
        let scheduler = SCHEDULER.lock();
        validate_capture(&scheduler, capture);
        previous_thread = scheduler.thread(capture.current()).map_or(0, |thread| thread.id);
        for (slot, thread) in scheduler.iter()
        {
            if !eligible(slot, thread.state.get(), capture)
            {
                continue;
            }
            if scratch.records.len() == wire::MAX_CANDIDATES
            {
                return Err((NOT_SUPPORTED,
                            "complete eligible set exceeds the admitted policy capacity; no \
                             candidates were truncated")
                                                        .into());
            }
            let record =
                thread.policy_record(now_microseconds).map_err(|cause| {
                                                           serial!("SCHEDULER_FACTS_REFUSED \
                                                                    thread={} cause={cause}",
                                                                   thread.id);
                                                           (cause.status(),
                                                            "canonical thread timing facts are \
                                                             unavailable or invalid")
                                                       })?;
            // Capacity for the entire supported set was admitted by PREPARE.
            scratch.records.push(record);
        }
        if capture.hart().is_primary()
        {
            // The complete immutable Round now contains these runnable edges.
            // A new edge after this guard drops must retain its own hint even
            // if the older returned choice still passes live revalidation.
            super::wake::observed_runnable();
        }
    }
    scratch.sequence =
        scratch.sequence
               .checked_add(1)
               .ok_or((NOT_SUPPORTED,
                       "policy round sequence exhausted; registration must retire"))?;
    let context =
        wire::Context { generation: pin.generation(),
                        round: scratch.sequence,
                        hart: capture.hart().number() as u64,
                        now_microseconds,
                        previous_thread,
                        cause: wire::Cause::Handoff,
                        minimum_quantum_microseconds: plan.limits.minimum_quantum_microseconds
                                                      as u32,
                        maximum_quantum_microseconds: plan.limits.maximum_quantum_microseconds
                                                      as u32 };
    let packet =
        wire::encode_round(context, &scratch.records, &mut scratch.packet).map_err(|_| {
            (BAD_ABI, "canonical scheduling snapshot cannot encode its admitted complete set")
        })?;
    // All SCHEDULER guards, iterators and mutable frame borrows ended above.
    let decision = match crate::drivers::choose_scheduler(&lease, packet)
    {
        Ok(decision) => decision,
        Err(crate::drivers::SchedulerFailure::Late { elapsed,
                                                     budget }) =>
        {
            // An elapsed guest-counter interval also includes virtualization
            // pauses. Reject this stale sample without declaring a corrupt
            // module. No late decision is committed; refresh work runs below.
            if !plan.late_reported.swap(true, Ordering::Relaxed)
            {
                serial!("SCHEDULER_SAMPLE_LATE generation={} elapsed_us={} budget_us={}; \
                         decision discarded, bootstrap used for this round, registration retained",
                        pin.generation(),
                        elapsed,
                        budget);
            }
            return Err(Rejection::Expired);
        }
        Err(cause) =>
        {
            serial!("SCHEDULER_CALLBACK_REFUSED generation={} cause={cause}", pin.generation());
            return Err((cause.status(), "policy callback failed; original registration retained for retirement").into());
        }
    };
    let quantum_ticks = if decision.thread().is_some()
    {
        crate::machine::monitor::microseconds_to_ticks_ceil(decision.quantum().as_micros() as u64)
            .map_err(|_| (BAD_ABI, "chosen quantum cannot be represented by the admitted timer"))?
    }
    else
    {
        super::timer_interval()
    };
    let selection = Selection { thread: decision.thread(),
                                quantum_ticks };
    {
        let scheduler = SCHEDULER.lock();
        validate_capture(&scheduler, capture);
        if selection.slot(&scheduler, capture).is_none()
        {
            return Err((SERVICE_BUSY,
                        "chosen thread readiness or running claim changed before commitment").into());
        }
    }
    // Callback and scratch leases end before any publication/retirement work.
    drop(loan);
    drop(lease);
    Ok(selection)
}
