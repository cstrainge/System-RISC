//! Replaceable scheduling policy consumes canonical THRD facts; the native
//! core retains frame/claim authority. Per-hart callback storage is admitted
//! before takeover; each final choice retains its actual policy generation.
#![forbid(unsafe_code)]
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::{ACCESS_DENIED, BAD_ABI, NO_MEMORY, NOT_SUPPORTED, SERVICE_BUSY, Status};
use mac_scheduler_interface::{self as wire, control};
use spinning_top::Spinlock;

// The quantum and late-return allowance are independently checked platform
// choices. Neither is a latency guarantee or recovery from a nonreturning call.
use super::{CALLBACK_BUDGET_MICROSECONDS, QUANTUM_MICROSECONDS};
use super::{IDLE, SCHEDULER, Scheduler, validate_capture};
use crate::{machine::traps::{OuterCapture,
                             execution::{HartBinding, HartKey}},
            toolbox::handles::OwnerRef};

pub(crate) fn limits() -> Result<control::Limits, Status>
{
    let cpus = crate::machine::paging::enabled_cpus().ok_or(BAD_ABI)?;
    let boot_hart = crate::machine::paging::firmware_owner_hart().ok_or(BAD_ABI)? as u64;
    if crate::machine::traps::execution::prepared_harts().is_none_or(|harts| {
                                                             harts.len() != cpus.len()
                                                         })
    {
        return Err(BAD_ABI);
    }
    let physical_harts = cpus.len().try_into().map_err(|_| NOT_SUPPORTED)?;
    let result =
        control::Limits { features: control::TAKEOVER
                                    | if cpus.len() > 1 { control::MULTI_HART } else { 0 },
                          maximum_candidates: wire::MAX_CANDIDATES as u32,
                          maximum_active_harts: physical_harts,
                          minimum_quantum_microseconds: QUANTUM_MICROSECONDS,
                          maximum_quantum_microseconds: QUANTUM_MICROSECONDS,
                          callback_budget_microseconds: CALLBACK_BUDGET_MICROSECONDS,
                          physical_harts,
                          active_harts:
                              crate::machine::smp::active_harts().try_into().map_err(|_| BAD_ABI)?,
                          boot_hart,
                          // The admitted boot topology is immutable until reset. No hotplug
                          // producer exists; requests from a different epoch are refused.
                          topology_epoch: 1 };
    result.validate().map_err(|_| BAD_ABI)?;
    Ok(result)
}

pub(crate) struct Plan
{
    limits:              control::Limits,
    scratch:             Vec<HartScratch>,
    late_reported:       AtomicBool,
    transition_reported: AtomicBool
}
struct HartScratch
{
    binding:          HartBinding,
    value:            Spinlock<Option<Scratch>>,
    reports:          Spinlock<Reports>,
    failure_reported: AtomicBool
}
struct Scratch
{
    records:  Vec<wire::Record>,
    packet:   Vec<u8>,
    sequence: u64
}

#[derive(Default)]
struct Reports
{
    failure:    Option<FailureReport>,
    late:       Option<LateReport>,
    transition: Option<TransitionReport>
}
impl Reports
{
    fn is_empty(&self) -> bool
    {
        self.failure.is_none() && self.late.is_none() && self.transition.is_none()
    }
}
struct FailureReport
{
    generation: u64,
    failure:    Failure
}
struct LateReport
{
    generation: u64,
    elapsed:    u64,
    budget:     u64
}
struct TransitionReport
{
    generation: u64,
    status:     Status,
    shadow:     bool
}
struct Failure
{
    status:  Status,
    detail:  &'static str,
    context: FailureContext
}
enum FailureContext
{
    Core,
    Callback(crate::drivers::SchedulerFailure),
    Facts
    {
        thread: u64,
        cause:  super::RunFactsError
    }
}
impl core::fmt::Display for FailureContext
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Core => out.write_str("canonical policy admission/snapshot"),
            Self::Callback(cause) => cause.fmt(out),
            Self::Facts { thread,
                          cause } => write!(out, "thread={thread} {cause}")
        }
    }
}
impl Plan
{
    /// Runtime has selected this actual module's Kernel account before entry.
    /// Nothing is published unless every real row's complete-set buffers can
    /// be admitted. No callback round grows a record or packet allocation.
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
        let harts = crate::machine::traps::execution::prepared_harts().ok_or(BAD_ABI)?;
        let count = harts.len();
        if count == 0 || count != limits.physical_harts as usize
        {
            return Err(BAD_ABI);
        }
        let mut scratch = Vec::new();
        scratch.try_reserve_exact(count).map_err(|_| NO_MEMORY)?;
        for (ordinal, binding) in harts.enumerate()
        {
            if binding.ordinal() != ordinal
            {
                return Err(BAD_ABI);
            }
            let mut records = Vec::new();
            records.try_reserve_exact(wire::MAX_CANDIDATES).map_err(|_| NO_MEMORY)?;
            let mut packet = Vec::new();
            packet.try_reserve_exact(wire::MAX_ROUND_BYTES).map_err(|_| NO_MEMORY)?;
            packet.resize(wire::MAX_ROUND_BYTES, 0);
            scratch.push(HartScratch { binding,
                                       value: Spinlock::new(Some(Scratch { records,
                                                                           packet,
                                                                           sequence: 0 })),
                                       reports: Spinlock::new(Reports::default()),
                                       failure_reported: AtomicBool::new(false) });
        }
        Ok(Self { limits,
                  late_reported: AtomicBool::new(false),
                  transition_reported: AtomicBool::new(false),
                  scratch })
    }

    pub(crate) fn limits(&self) -> control::Limits
    {
        self.limits
    }

    /// The adapter has already withdrawn new leases and bootstrap-selects.
    /// All peer dispatch grants must have drained and every actual per-row
    /// loan must be returned before the plan's backing may be disposed.
    pub(crate) fn retire(&self) -> Result<bool, Status>
    {
        Ok(super::drain_policy_harts()
           && self.scratch
                  .iter()
                  .all(|row| row.value.lock().is_some() && row.reports.lock().is_empty()))
    }

    /// Primary maintenance calls this with no phase/catalog/SCHEDULER guard.
    /// Actual peer reports stay prepaid and queued until the fixed firmware
    /// owner drains them; recording never enters the opaque serial callback.
    pub(crate) fn report(&self)
    {
        if !crate::machine::traps::execution::selected_hart().is_primary()
        {
            crate::drivers::scheduler::report_pending();
            return;
        }
        for row in &self.scratch
        {
            let reports = {
                let mut stored = row.reports.lock();
                core::mem::take(&mut *stored)
            };
            if let Some(LateReport { generation,
                                     elapsed,
                                     budget }) = reports.late
            {
                serial!("SCHEDULER_SAMPLE_LATE generation={} hart={} elapsed_us={} budget_us={}; \
                         decision discarded, bootstrap used for this round, registration retained",
                        generation,
                        row.binding.number(),
                        elapsed,
                        budget);
            }
            if let Some(FailureReport { generation,
                                        failure }) = reports.failure
            {
                serial!("SCHEDULER_POLICY_REFUSED generation={} hart={} status={} cause={}; \
                         context={}",
                        generation,
                        row.binding.number(),
                        failure.status,
                        failure.detail,
                        failure.context);
            }
            if let Some(TransitionReport { generation,
                                           status,
                                           shadow }) = reports.transition
            {
                if shadow
                {
                    serial!("SCHEDULER_SHADOW_COMMIT_REFUSED generation={} hart={} status={}; \
                             bootstrap retained",
                            generation,
                            row.binding.number(),
                            status);
                }
                else
                {
                    serial!("SCHEDULER_POLICY_RETIRE_RETAINED generation={} hart={} status={}",
                            generation,
                            row.binding.number(),
                            status);
                }
            }
        }
    }

    fn report_row(&self, hart: HartKey) -> &HartScratch
    {
        // A Plan is made only from the permanent registry and authenticated
        // captured keys. Losing that binding is a System entry invariant, not
        // a module protocol error or a reason to forge a fallback row.
        match self.scratch.get(hart.ordinal()).filter(|row| row.binding.matches(hart))
        {
            Some(row) => row,
            None => crate::machine::traps::entry_lost("policy report lost its genuine hart binding")
        }
    }

    fn record_failure(&self, hart: HartKey, generation: u64, failure: Failure)
    {
        let row = self.report_row(hart);
        if !row.failure_reported.swap(true, Ordering::AcqRel)
        {
            {
                let mut reports = row.reports.lock();
                reports.failure = Some(FailureReport { generation,
                                                       failure });
            }
            crate::drivers::scheduler::report_pending();
        }
    }

    fn record_late(&self, hart: HartKey, generation: u64, elapsed: u64, budget: u64)
    {
        if !self.late_reported.swap(true, Ordering::AcqRel)
        {
            {
                let mut reports = self.report_row(hart).reports.lock();
                reports.late = Some(LateReport { generation,
                                                 elapsed,
                                                 budget });
            }
            crate::drivers::scheduler::report_pending();
        }
    }

    fn record_transition(&self, hart: HartKey, generation: u64, status: Status, shadow: bool)
    {
        if !self.transition_reported.swap(true, Ordering::AcqRel)
        {
            {
                let mut reports = self.report_row(hart).reports.lock();
                reports.transition = Some(TransitionReport { generation,
                                                             status,
                                                             shadow });
            }
            crate::drivers::scheduler::report_pending();
        }
    }

    fn scratch(&self, hart: HartKey) -> Result<ScratchLoan<'_>, Status>
    {
        // The ordinal is an O(1) container coordinate, never authority. Bind it
        // to the same actual immutable registry row as the captured HartKey.
        let row = self.scratch.get(hart.ordinal()).ok_or(BAD_ABI)?;
        if !row.binding.matches(hart)
        {
            return Err(BAD_ABI);
        }
        let scratch = row.value.lock().take().ok_or(SERVICE_BUSY)?;
        Ok(ScratchLoan { row,
                         scratch: Some(scratch) })
    }
}

/// The only writer of an empty scratch slot is the loan that took its value.
/// No packet or scratch lock crosses module code. Refusal returns the same
/// admitted buffers for later retry or real disposal.
struct ScratchLoan<'a>
{
    row:     &'a HartScratch,
    scratch: Option<Scratch>
}
impl Drop for ScratchLoan<'_>
{
    fn drop(&mut self)
    {
        // The non-Clone loan is the only writer of this privately empty slot.
        // Safe callers can borrow its buffers but cannot take or duplicate them.
        *self.row.value.lock() = self.scratch.take();
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
    policy:     crate::drivers::scheduler::PolicyPin,
    generation: u64,
    hart:       HartKey,
    choice:     Choice
}
impl Selection
{
    /// The caller holds the actual SCHEDULER turn. The adapter's phase gate is
    /// held across live eligibility validation AND the nonforeign THRD claim.
    /// Keep this Selection owner until AFTER that scheduler guard drops: the
    /// last policy Arc may dispose its prepaid buffers/provider pins.
    pub(super) fn commit<R>(&self, scheduler: &Scheduler, capture: &OuterCapture,
                            operation: impl FnOnce(usize, u64) -> Option<R>)
                            -> Result<Option<R>, Status>
    {
        if self.hart != capture.hart()
        {
            crate::machine::traps::entry_lost("policy selection changed its genuine captured hart");
        }
        crate::drivers::scheduler::with_active_policy(&self.policy, self.generation, || {
            self.choice
                .slot(scheduler, capture)
                .and_then(|slot| operation(slot, self.choice.quantum_ticks))
        })
    }
}
struct Choice
{
    thread:        Option<u64>,
    quantum_ticks: u64
}
impl Choice
{
    fn slot(&self, scheduler: &Scheduler, capture: &OuterCapture) -> Option<usize>
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
    && state.allows(capture.hart())
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
    Contended,
    Permanent(Failure)
}
impl From<(Status, &'static str)> for Rejection
{
    fn from((status, detail): (Status, &'static str)) -> Self
    {
        Self::Permanent(Failure { status,
                                  detail,
                                  context: FailureContext::Core })
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
    let generation = pin.generation();
    let result = select_from(&pin, generation, capture);
    let selection = match result
    {
        Ok(selection) => selection,
        Err(Rejection::Expired) => return Err(Expired),
        Err(Rejection::Contended) => return Ok(None),
        Err(Rejection::Permanent(failure)) =>
        {
            let status = failure.status;
            pin.plan().record_failure(capture.hart(), generation, failure);
            if let Err(retained) = adapter::refuse(&pin, status)
            {
                pin.plan().record_transition(capture.hart(), generation, retained, false);
            }
            return Ok(None);
        }
    };
    // Only the actual primary may publish a successful Armed shadow. The
    // original pin survives that publication and is checked again jointly with
    // the eventual canonical THRD claim, not by an earlier current() snapshot.
    if shadow
    {
        if !capture.hart().is_primary()
        {
            return Ok(None);
        }
        if let Err(status) = adapter::commit_shadow(ActivationProof { policy: pin.clone() })
        {
            if status != SERVICE_BUSY
            {
                pin.plan().record_transition(capture.hart(), generation, status, true);
            }
            return Ok(None);
        }
    }
    Ok(Some(Selection { policy: pin,
                        generation,
                        hart: capture.hart(),
                        choice: selection }))
}

fn select_from(pin: &crate::drivers::scheduler::PolicyPin, generation: u64, capture: &OuterCapture)
               -> Result<Choice, Rejection>
{
    let lease = crate::drivers::scheduler::begin_call(pin).map_err(|status| {
                                                              if matches!(status,
                                                                          SERVICE_BUSY
                                                                          | mac_abi::NOT_FOUND)
                                                              {
                                                                  Rejection::Contended
                                                              }
                                                              else
                                                              {
                                                                  (status,
                                                                   "original policy registration \
                                                                    cannot admit a callback")
                                                                                             .into()
                                                              }
                                                          })?;
    let plan = lease.plan();
    let mut loan = plan.scratch(capture.hart()).map_err(|status| {
                                                    if status == SERVICE_BUSY
                                                    {
                                                        Rejection::Contended
                                                    }
                                                    else
                                                    {
                                                        (status,
                                                         "policy scratch does not bind the actual \
                                                          captured hart")
                                                                         .into()
                                                    }
                                                })?;
    let scratch =
        loan.scratch.as_mut().ok_or((BAD_ABI, "policy scratch loan has no retained buffers"))?;
    scratch.records.clear();
    let now_microseconds;
    let previous_thread;
    let mut facts_failure = None;
    {
        let scheduler = SCHEDULER.lock();
        validate_capture(&scheduler, capture);
        // Ready timestamps and this time sample belong to the SAME canonical
        // turn. A peer wake between an earlier sample and lock acquisition
        // must not become a false future-ready/protocol failure.
        now_microseconds = crate::machine::monitor::ticks_to_microseconds(crate::platform::ticks())
            .map_err(|_| (BAD_ABI, "admitted clock cannot represent this scheduler round"))?;
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
            let record = match thread.policy_record(now_microseconds)
            {
                Ok(record) => record,
                Err(cause) =>
                {
                    facts_failure = Some((thread.id, cause));
                    break;
                }
            };
            // Capacity for the entire supported set was admitted by PREPARE.
            scratch.records.push(record);
        }
        if facts_failure.is_none()
        {
            // The complete immutable Round now contains these runnable edges.
            // A new edge after this guard drops must retain its own hint even
            // if the older returned choice still passes live revalidation.
            if capture.hart().is_primary()
            {
                super::wake::observed_runnable();
            }
            else
            {
                crate::machine::smp::observed_runnable(capture.hart());
            }
        }
    }
    if let Some((thread, cause)) = facts_failure
    {
        return Err(Rejection::Permanent(Failure { status:  cause.status(),
                                                  detail:  "canonical thread timing facts are \
                                                            unavailable or invalid",
                                                  context: FailureContext::Facts { thread,
                                                                                   cause } }));
    }
    scratch.sequence =
        scratch.sequence
               .checked_add(1)
               .ok_or((NOT_SUPPORTED,
                       "policy round sequence exhausted; registration must retire"))?;
    let context =
        wire::Context { generation,
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
            plan.record_late(capture.hart(), generation, elapsed, budget);
            return Err(Rejection::Expired);
        }
        Err(crate::drivers::SchedulerFailure::Admission(SERVICE_BUSY)
            | crate::drivers::SchedulerFailure::Provider(SERVICE_BUSY)) =>
        {
            return Err(Rejection::Contended);
        }
        Err(cause) =>
        {
            return Err(Rejection::Permanent(Failure { status:  cause.status(),
                                                      detail:  "policy callback failed; original \
                                                                registration retained for \
                                                                retirement",
                                                      context: FailureContext::Callback(cause) }));
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
    let selection = Choice { thread: decision.thread(),
                             quantum_ticks };
    {
        let scheduler = SCHEDULER.lock();
        validate_capture(&scheduler, capture);
        if selection.slot(&scheduler, capture).is_none()
        {
            // Another real hart can legitimately claim/retire this candidate
            // after the copied Round. No protocol fault or policy withdrawal.
            return Err(Rejection::Contended);
        }
    }
    // Callback and scratch leases end before any publication/retirement work.
    drop(loan);
    drop(lease);
    Ok(selection)
}
