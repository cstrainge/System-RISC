//! Native termination is an owned Process protocol. Failure admission uses the
//! slot prepared with that Process, while a guard-free System pump owns report
//! and shell-replacement progress. It never resumes a failed author to retry.
#![forbid(unsafe_code)]
use core::{fmt,
           sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}};

use mac_abi::{processes::*, *};
use mac_object_table::shared::InvocationGate as _;
use spinning_top::Spinlock;

use super::ResourceBinding;
use crate::{machine::scheduler::{self,
                                 service::{RequestAction, RequestResult}},
            toolbox::handles::{InvocationGate, OwnerRef}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome
{
    Normal(i32),
    Abort(i32),
    Interrupted,
    Legacy,
    ShellReplacement
}
impl fmt::Display for Outcome
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Normal(code) => write!(out, "normal code={code}"),
            Self::Abort(code) => write!(out, "abort code={code}"),
            Self::Interrupted => out.write_str("terminal-interrupt"),
            Self::Legacy => out.write_str("legacy"),
            Self::ShellReplacement => out.write_str("shell-replacement")
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Retention
{
    NotRequested,
    CapturedComplete,
    CapturedPrefix,
    SystemDiagnostic,
    PreviousComplete,
    PreviousPrefix,
    PreviousSystemDiagnostic
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct Record
{
    pub(crate) outcome:       Outcome,
    pub(crate) thread:        u64,
    pub(crate) report:        Retention,
    pub(crate) report_app:    u64,
    pub(crate) report_thread: u64,
    pub(crate) fault:         Option<scheduler::FaultSnapshot>
}
impl fmt::Display for Record
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        write!(out,
               "kind={} thread={} report={:?} report_app={} report_thread={}",
               self.outcome, self.thread, self.report, self.report_app, self.report_thread)
    }
}
#[derive(Default)]
pub(crate) struct State
{
    record:  Option<Record>,
    pending: Option<Pending>
}
impl State
{
    pub(crate) fn record(&self) -> Option<Record>
    {
        self.record
    }

    pub(crate) fn stopped(&mut self, outcome: Outcome, thread: u64)
    {
        self.record.get_or_insert(Record { outcome,
                                           thread,
                                           report: Retention::NotRequested,
                                           report_app: 0,
                                           report_thread: 0,
                                           fault: None });
    }

    pub(crate) fn reported(&mut self, report: Retention, app: u64, thread: u64)
    {
        if let Some(record) = self.record.as_mut()
        {
            record.report = report;
            record.report_app = app;
            record.report_thread = thread;
        }
    }
}

enum Phase
{
    Quiesce,
    Claim,
    Report(crate::recovery::Activity),
    Dismiss(crate::recovery::Activity)
}
struct Pending
{
    binding: ResourceBinding,
    record:  Record,
    phase:   Phase,
    last:    Option<Status>,
    adopted: Option<crate::toolbox::crashes::Snapshot>,
    fresh:   Option<crate::recovery::FreshReport>
}
static COUNT: AtomicUsize = AtomicUsize::new(0);
static VISITS: AtomicUsize = AtomicUsize::new(0);
static CURSOR: AtomicU64 = AtomicU64::new(0);
static PREFERRED_OWNER: AtomicU64 = AtomicU64::new(0);
static WAKE: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);
static WORK: Spinlock<Option<Pending>> = Spinlock::new(None);
impl Drop for Pending
{
    fn drop(&mut self)
    {
        COUNT.fetch_sub(1, Ordering::AcqRel);
    }
}
pub(crate) fn progress()
{
    let count = COUNT.load(Ordering::Acquire);
    if count != 0
    {
        VISITS.fetch_max(count, Ordering::AcqRel);
        again();
    }
}
fn again()
{
    if !WAKE.swap(true, Ordering::AcqRel)
    {
        scheduler::wake::request();
    }
}

/// The scheduler alone mints this authority from a hardware user-fault capture.
/// A trusted maintenance turn can continue its admission after the author has
/// parked; it does not impersonate that application's running thread.
pub(crate) fn queue_user_fault(fault: &scheduler::UserFault) -> Result<(), Status>
{
    let owner = fault.owner();
    if !owner.accepting()
    {
        // The canonical owner walk already owns stopping this incarnation.
        return Ok(());
    }
    let tls = crate::kernel::with_toolbox(|toolbox| {
                  toolbox.processes.native_exit_preflight(owner, fault.image())
              }).map_err(|cause| cause.status())??;
    let runtime =
        crate::toolbox::native_threads::runtime_admission(&tls, owner).map_err(|cause| {
                                                                          cause.status()
                                                                      })?;
    let outcome = Outcome::Abort(fault.snapshot().abort_code());
    let first = crate::kernel::with_toolbox(|toolbox| {
                    toolbox.processes.queue_native_failure(owner,
                                                           fault.image(),
                                                           fault.thread().0,
                                                           outcome,
                                                           Some(fault.snapshot()))
                }).map_err(|cause| cause.status())??;
    if let Err(cause) = scheduler::application::fail_owner(&runtime)
    {
        if first
        {
            let refused = crate::kernel::with_toolbox(|toolbox| {
                              toolbox.processes
                                     .rollback_native_failure(owner, fault.thread().0, outcome)
                          }).map_err(|cause| cause.status())??;
            drop(refused);
        }
        return Err(cause);
    }
    drop(runtime);
    crate::recovery::owner_failed(owner);
    progress();
    Ok(())
}
struct Running;
impl Drop for Running
{
    fn drop(&mut self)
    {
        RUNNING.store(false, Ordering::Release);
    }
}

/// This is the size-distinct PROCESS handler used by the real outer native
/// action dispatcher. All guards end before RequestAction::Exit is consumed.
pub(crate) fn request(call: &mut NativeExitCall, suspension: Result<(), Status>) -> RequestResult
{
    let result =
        (|| {
            if call.reserved != 0 || !matches!(call.flags, NATIVE_EXIT_NORMAL | NATIVE_EXIT_ABORT)
            {
                return Err(BAD_ARGUMENT);
            }
            suspension?;
            scheduler::service::exit_allowed()?;
            crate::context::with_application_executor(|executor| {
                let owner = executor.owner();
                let outcome = if call.flags == NATIVE_EXIT_NORMAL
                {
                    Outcome::Normal(call.code)
                }
                else
                {
                    Outcome::Abort(call.code)
                };
                let (joined, main_error) =
                    crate::kernel::with_toolbox(|toolbox| {
                        let joined = toolbox.processes
                                            .get(owner.owner().incarnation())?
                                            .termination
                                            .record
                                            .is_some();
                        let main_error = toolbox.main_error_report(owner.owner().incarnation(),
                                                                   executor.thread());
                        Ok::<_, Status>((joined, main_error))
                    }).map_err(|error| error.status())??;
                let failure = joined
                              || call.flags == NATIVE_EXIT_ABORT
                              || (call.code != 0 && main_error)
                              || owner.owner().incarnation() == crate::context::finder_app().0;
                if !failure
                {
                    return crate::kernel::with_toolbox(|toolbox| {
                               toolbox.native_normal_exit(owner,
                                                          executor.image(),
                                                          executor.thread(),
                                                          outcome)
                           }).map_err(|error| error.status())?;
                }
                // Resolve the real registered runtime root before any admission
                // effect. A source/app number cannot construct RuntimeAdmission.
                let tls = crate::kernel::with_toolbox(|toolbox| {
                              toolbox.processes.native_exit_preflight(owner, executor.image())
                          }).map_err(|error| error.status())??;
                let runtime = crate::toolbox::native_threads::runtime_admission(&tls, owner)
                .map_err(|cause| cause.status())?;
                let first = crate::kernel::with_toolbox(|toolbox| {
                                toolbox.processes.queue_native_failure(owner,
                                                                       executor.image(),
                                                                       executor.thread(),
                                                                       outcome,
                                                                       None)
                            }).map_err(|error| error.status())??;
                // Once accepted, this closes child birth and parks this genuine
                // quiescent caller atomically under SCHED. Other Host continuations
                // drain their actual leases; no image/stack reclamation is asserted.
                if let Err(cause) = scheduler::application::fail_owner(&runtime)
                {
                    // Refusal precedes runtime-gate effects. Undo only our own
                    // unpublished preparation; a join never erases the first
                    // accepted request. Drop the detached row outside Desktop.
                    if first
                    {
                        let refused =
                            crate::kernel::with_toolbox(|toolbox| {
                                toolbox.processes
                                       .rollback_native_failure(owner, executor.thread(), outcome)
                            }).map_err(|error| error.status())??;
                        drop(refused);
                    }
                    return Err(cause);
                }
                drop(runtime);
                crate::recovery::owner_failed(owner);
                if first
                {
                    serial!("NATIVE_EXIT_PENDING app={} thread={} kind={outcome} \
                             state=author-quiescence",
                            owner.owner().incarnation(),
                            executor.thread());
                }
                progress();
                Ok(())
            }).map_err(|cause| cause.status())?
        })();
    match result
    {
        Ok(()) =>
        {
            RequestResult { status: OK,
                            action: RequestAction::Exit }
        }
        Err(status) =>
        {
            serial!("NATIVE_EXIT_REFUSED flags={} code={} status={status}; no author-success \
                     result was returned",
                    call.flags,
                    call.code);
            RequestResult::done(status)
        }
    }
}

impl super::Manager
{
    pub(crate) fn exit_record(&self, id: u64) -> Result<Option<Record>, Status>
    {
        Ok(self.get(id)?.termination.record())
    }

    pub(crate) fn note_exit(&mut self, owner: &OwnerRef, outcome: Outcome, thread: u64)
                            -> Result<(), Status>
    {
        let id = owner.owner().incarnation();
        if self.ledger(id)?.owner() != owner.owner()
        {
            return Err(ACCESS_DENIED);
        }
        let process = self.get_mut(id)?;
        process.termination.stopped(outcome, thread);
        let result = process.termination.record().map(|record| record.outcome);
        if let Some(parent) = process.parent
           && let Ok(parent) = self.get_mut(parent)
           && let Some(receipt) = parent.children.iter_mut().find(|receipt| receipt.id == id)
        {
            receipt.outcome = result;
            scheduler::wake_child(id);
        }
        Ok(())
    }

    pub(crate) fn note_report(&mut self, binding: &ResourceBinding, retention: Retention,
                              app: u64, thread: u64)
                              -> Result<Record, Status>
    {
        if self.ledger(binding.owner())?.owner() != binding.ledger().owner()
        {
            return Err(ACCESS_DENIED);
        }
        let state = &mut self.get_mut(binding.owner())?.termination;
        state.reported(retention, app, thread);
        state.record().ok_or(BAD_ABI)
    }

    pub(crate) fn native_exit_preflight(
        &self, owner: &OwnerRef, image: crate::images::Binding)
        -> Result<crate::toolbox::native_threads::RootBinding, Status>
    {
        let binding = self.resource_binding(owner.owner().incarnation())?;
        if binding.ledger().owner() != owner.owner()
           || binding.image().map(crate::images::Binding::id) != Some(image.id())
           || !owner.accepting()
        {
            return Err(ACCESS_DENIED);
        }
        self.tls(owner.owner().incarnation())
    }

    fn queue_native_failure(&mut self, owner: &OwnerRef, image: crate::images::Binding,
                            thread: u64, outcome: Outcome, fault: Option<scheduler::FaultSnapshot>)
                            -> Result<bool, Status>
    {
        let id = owner.owner().incarnation();
        let binding = self.resource_binding(id)?;
        if binding.ledger().owner() != owner.owner()
           || binding.image().map(crate::images::Binding::id) != Some(image.id())
           || !owner.accepting()
        {
            return Err(ACCESS_DENIED);
        }
        let state = &mut self.get_mut(id)?.termination;
        if state.record.is_some()
        {
            return Ok(false);
        }
        COUNT.try_update(Ordering::AcqRel, Ordering::Acquire, |count| count.checked_add(1))
             .map_err(|_| NO_MEMORY)?;
        let record = Record { outcome,
                              thread,
                              report: Retention::NotRequested,
                              report_app: 0,
                              report_thread: 0,
                              fault };
        state.pending = Some(Pending { binding,
                                       record,
                                       phase: Phase::Quiesce,
                                       last: None,
                                       adopted: None,
                                       fresh: None });
        state.record = Some(record);
        Ok(true)
    }

    fn rollback_native_failure(&mut self, owner: &OwnerRef, thread: u64, outcome: Outcome)
                               -> Result<Option<Pending>, Status>
    {
        let binding = self.resource_binding(owner.owner().incarnation())?;
        if binding.ledger().owner() != owner.owner()
        {
            return Err(ACCESS_DENIED);
        }
        let state = &mut self.get_mut(binding.owner())?.termination;
        if state.pending.as_ref().is_none_or(|pending| {
                                     pending.record.thread != thread
                                     || pending.record.outcome != outcome
                                     || !matches!(pending.phase, Phase::Quiesce)
                                 })
        {
            return Err(SERVICE_BUSY);
        }
        state.record = None;
        Ok(state.pending.take())
    }

    pub(crate) fn has_native_failure(&self, id: u64) -> bool
    {
        self.get(id).is_ok_and(|process| process.termination.pending.is_some())
    }

    fn take_native_failure(&mut self, preferred: Option<u64>, after: u64) -> Option<Pending>
    {
        if let Some(id) = preferred
           && let Ok(process) = self.get_mut(id)
           && let Some(pending) = process.termination.pending.take()
        {
            return Some(pending);
        }
        // Facility table order is not numeric generation-ID order. Choose
        // first, then take once; a reused high-generation slot cannot starve
        // lower IDs when the bounded wave wraps.
        let next = self.entries
                       .iter()
                       .filter(|entry| entry.value().termination.pending.is_some())
                       .map(|entry| entry.owner().incarnation())
                       .filter(|id| *id > after)
                       .min();
        let id = match next
        {
            Some(id) => id,
            None =>
            {
                self.entries
                    .iter()
                    .filter(|entry| entry.value().termination.pending.is_some())
                    .map(|entry| entry.owner().incarnation())
                    .min()?
            }
        };
        self.get_mut(id).ok()?.termination.pending.take()
    }

    fn restore_native_failure(&mut self, pending: Pending) -> Result<(), Pending>
    {
        if self.ledger(pending.binding.owner())
               .is_ok_and(|owner| owner.owner() == pending.binding.ledger().owner())
           && let Ok(process) = self.get_mut(pending.binding.owner())
           && process.termination.pending.is_none()
        {
            process.termination.pending = Some(pending);
            Ok(())
        }
        else
        {
            Err(pending)
        }
    }
}

fn toolbox<T>(pending: &Pending,
              action: impl FnOnce(&mut crate::toolbox::runtime::Toolbox) -> Result<T, Status>)
              -> Result<T, Status>
{
    crate::kernel::with_toolbox(|toolbox| {
        toolbox.native_failure_selection(&pending.binding)?;
        action(toolbox)
    }).map_err(|error| error.status())?
}
fn step(pending: &mut Pending) -> Result<bool, Status>
{
    if !pending.binding.ledger().accepting()
    {
        return Ok(true);
    }
    if !matches!(pending.phase, Phase::Quiesce)
    {
        let acknowledged = toolbox(pending, |toolbox| {
            if let Some(retention) = toolbox.acknowledged_native_failure(&pending.binding)?
            {
                let record = toolbox.record_native_report(&pending.binding, retention)?;
                toolbox.stop_native_failure(&pending.binding)?;
                Ok(Some(record))
            }
            else
            {
                Ok(None)
            }
        })?;
        if let Some(record) = acknowledged
        {
            pending.record = record;
            serial!("NATIVE_EXIT_REPORT_ACKNOWLEDGED app={} {} state=canonical-stop",
                    pending.binding.owner(),
                    pending.record);
            return Ok(true);
        }
    }
    match &pending.phase
    {
        Phase::Quiesce =>
        {
            // The initiating trap closed the genuine runtime gate before
            // returning its Exit action. Only actual claim/Host release can
            // advance this stage; there is no second root or guessed timer.
            if scheduler::application::failure_quiescent(pending.binding.ledger())?
            {
                if pending.record.outcome == Outcome::Interrupted
                {
                    // Interruption is a requested terminal outcome, not a
                    // crash. Use the same actual quiescence and cleanup gates
                    // without manufacturing a crash report or modal dialog.
                    toolbox(pending, |toolbox| toolbox.stop_native_failure(&pending.binding))?;
                    return Ok(true);
                }
                pending.phase = Phase::Claim;
                again();
            }
            Ok(false)
        }
        Phase::Claim =>
        {
            // Take the queued publisher before claiming the modal. Otherwise
            // its own retry could join/close our new session before adoption.
            if pending.adopted.is_none()
            {
                pending.adopted = crate::toolbox::crashes::take_for_termination(&pending.binding)?;
            }
            let keeps = pending.adopted.is_some()
                        || toolbox(pending, |toolbox| {
                            toolbox.native_failure_keeps_stock(&pending.binding)
                        })?;
            if !keeps && !crate::toolbox::crashes::protected_stock_available()
            {
                return Err(SERVICE_BUSY);
            }
            let claim = crate::recovery::claim_failure_report(pending.binding.ledger().clone())?;
            let (activity, fresh) = claim.into_parts();
            pending.fresh = fresh;
            pending.phase = Phase::Report(activity);
            again();
            Ok(false)
        }
        Phase::Report(activity) =>
        {
            let activity = activity.clone();
            if activity.closing()
            {
                // Holding the fresh proof must not pin a closing old session
                // against its actual ACK/reuse barrier. Keep any owned snapshot.
                drop(pending.fresh.take());
                pending.phase = Phase::Claim;
                again();
                return Ok(false);
            }
            let keeps = pending.adopted.is_some()
                        || toolbox(pending, |toolbox| {
                            toolbox.native_failure_keeps_stock(&pending.binding)
                        })?;
            if !keeps && !crate::toolbox::crashes::protected_stock_available()
            {
                if !activity.has_outstanding_windows()
                {
                    activity.close();
                    drop(pending.fresh.take());
                    pending.phase = Phase::Claim;
                }
                return Err(SERVICE_BUSY);
            }
            // Preserve the unique queued snapshot if scope/selection refuses
            // before its actual move into the saved Manager.
            let mut adopted = pending.adopted.take();
            let mut fresh = pending.fresh.take();
            let mut reclaim = false;
            let result = activity.run(|| {
                                     toolbox(pending, |toolbox| {
                                         match toolbox.adopt_native_failure_report(&pending.binding,
                                                                             &mut adopted,
                                                                             &mut fresh, &activity)?
                                         {
                                             crate::recovery::ReportAdoption::Ready => (),
                                             crate::recovery::ReportAdoption::FreshClaimNeeded => {
                                                 reclaim = true;
                                                 return Ok(None);
                                             }
                                         }
                                         let retained =
                                     toolbox.prepare_native_failure_report(&pending.binding,
                                                                           pending.record)?;
                                         toolbox.record_native_report(&pending.binding, retained)
                                                .map(Some)
                                     })
                                 });
            pending.adopted = adopted;
            pending.fresh = fresh;
            let record = result??;
            if reclaim
            {
                activity.close();
                drop(pending.fresh.take());
                pending.phase = Phase::Claim;
                again();
                return Ok(false);
            }
            let Some(record) = record
            else
            {
                return Err(BAD_ABI);
            };
            pending.record = record;
            if pending.binding.owner() == crate::context::finder_app().0
            {
                activity.run(|| {
                            toolbox(pending, |toolbox| {
                                toolbox.queue_native_finder_restart(pending.binding.retain())
                            })
                        })??;
                serial!("NATIVE_EXIT_RECOVERY_QUEUED app={} {} state=owned-finder-flight",
                        pending.binding.owner(),
                        pending.record);
                Ok(true)
            }
            else
            {
                pending.phase = Phase::Dismiss(activity);
                Ok(false)
            }
        }
        Phase::Dismiss(activity) =>
        {
            let activity = activity.clone();
            let dismissed = activity.run(|| {
                                        toolbox(pending, |toolbox| {
                                            Ok(toolbox.native_failure_dismissed(&pending.binding)?)
                                        })
                                    })??;
            if !dismissed
            {
                return Ok(false);
            }
            // Drop/close the System report session before the canonical owner
            // walk; it owns no application callback or destructor completion.
            activity.close();
            toolbox(pending, |toolbox| toolbox.stop_native_failure(&pending.binding))?;
            Ok(true)
        }
    }
}
fn retain_work(mut pending: Option<Pending>, rotate: bool)
{
    if rotate
    {
        let restored = crate::kernel::with_toolbox(|toolbox| {
            match pending.take()
            {
                Some(work) => toolbox.processes.restore_native_failure(work).err(),
                None => None
            }
        });
        if let Ok(restored) = restored
        {
            pending = restored;
        }
    }
    if let Some(pending) = pending
    {
        InvocationGate::run(|| *WORK.lock() = Some(pending));
    }
    else if VISITS.load(Ordering::Acquire) != 0
    {
        again();
    }
}
/// One stage per maintenance turn. A wave visits a blocked no-Activity request
/// at most once per actual change; owning Report/Dismiss serializes the one
/// modal. An unrelated Host/claim refusal cannot hold every other failure back.
pub(crate) fn pump()
{
    scheduler::pump_faults();
    if !WAKE.load(Ordering::Acquire) || RUNNING.swap(true, Ordering::AcqRel)
    {
        return;
    }
    let _running = Running;
    WAKE.store(false, Ordering::Release);
    let pending = InvocationGate::run(|| WORK.lock().take());
    let pending = match pending
    {
        Some(pending) => Some(pending),
        None =>
        {
            if VISITS.try_update(Ordering::AcqRel, Ordering::Acquire, |left| left.checked_sub(1))
                     .is_err()
            {
                return;
            }
            match crate::kernel::with_toolbox(|toolbox| {
                      let stock = toolbox.native_failure_stock_owner();
                      let previous = PREFERRED_OWNER.swap(stock.unwrap_or(0), Ordering::Relaxed);
                      let preferred = stock.filter(|owner| *owner != previous);
                      toolbox.processes
                             .take_native_failure(preferred, CURSOR.load(Ordering::Relaxed))
                  })
            {
                Ok(pending) => pending,
                Err(_) =>
                {
                    progress();
                    return;
                }
            }
        }
    };
    let Some(mut pending) = pending
    else
    {
        return;
    };
    CURSOR.store(pending.binding.owner(), Ordering::Relaxed);
    let before = core::mem::discriminant(&pending.phase);
    let result = step(&mut pending);
    let changed = before != core::mem::discriminant(&pending.phase);
    match result
    {
        Ok(true) =>
        {
            drop(pending);
            if VISITS.load(Ordering::Acquire) != 0
            {
                again();
            }
        }
        Ok(false) =>
        {
            let rotate = !changed
                         && pending.adopted.is_none()
                         && pending.fresh.is_none()
                         && matches!(pending.phase, Phase::Quiesce | Phase::Claim);
            retain_work(Some(pending), rotate);
        }
        Err(status) =>
        {
            if pending.last != Some(status)
            {
                let stage = match pending.phase
                {
                    Phase::Quiesce => "author-quiescence",
                    Phase::Claim => "modal-claim",
                    Phase::Report(_) => "report-publication",
                    Phase::Dismiss(_) => "report-disposal"
                };
                serial!("NATIVE_EXIT_RETAINED app={} {} stage={stage} status={status}; original \
                         binding and termination intent retained; report publication incomplete",
                        pending.binding.owner(),
                        pending.record);
                pending.last = Some(status);
            }
            let rotate = pending.adopted.is_none()
                         && pending.fresh.is_none()
                         && matches!(pending.phase, Phase::Quiesce | Phase::Claim);
            retain_work(Some(pending), rotate);
        }
    }
}

/// Terminal-scoped interruption. The pipe manager supplies the non-forgeable
/// target set; every target must also belong to the authenticated host's CLAP
/// relation. Admission retains the original runtime root and cleanup protocol.
pub(crate) fn interrupt(targets: &crate::toolbox::pipes::InterruptTargets) -> Result<(), Status>
{
    // Resolve the CLAP relation from the authenticated input owners first.
    // Descendants can redirect every stream and still belong to this window;
    // endpoint enumeration alone would miss those members of the tree.
    let members = crate::kernel::with_toolbox(|toolbox| {
        let mut sessions = alloc::vec::Vec::new();
        InvocationGate::with_system_allocation(|| sessions.try_reserve(targets.owners().len()))
            .map_err(|_| NO_MEMORY)?;
        for &id in targets.owners()
        {
            let process = toolbox.processes.get(id)?;
            let session = process.console_session.ok_or(ACCESS_DENIED)?;
            if !process.console() || session.0 != targets.host() { return Err(ACCESS_DENIED); }
            if !sessions.contains(&session) { sessions.push(session); }
        }
        let mut members = alloc::vec::Vec::new();
        for entry in toolbox.processes.entries.iter()
        {
            let process = entry.value();
            if process.console_session.is_some_and(|session| sessions.contains(&session))
            {
                InvocationGate::with_system_allocation(|| members.try_reserve(1)).map_err(|_| NO_MEMORY)?;
                members.push(entry.owner().incarnation());
            }
        }
        Ok::<_, Status>(members)
    }).map_err(|cause| cause.status())??;
    let mut prepared = alloc::vec::Vec::new();
    InvocationGate::with_system_allocation(|| prepared.try_reserve(members.len())).map_err(|_| {
                                                                                      NO_MEMORY
                                                                                  })?;
    for id in members
    {
        let candidate = crate::kernel::with_toolbox(|toolbox| {
                            let process = toolbox.processes.get(id)?;
                            if !process.console()
                               || process.console_session
                                         .is_none_or(|session| session.0 != targets.host())
                            {
                                return Err(ACCESS_DENIED);
                            }
                            if process.termination.record.is_some()
                            {
                                return Ok(None);
                            }
                            let owner = toolbox.processes.ledger(id)?;
                            if !owner.accepting()
                            {
                                return Ok(None);
                            }
                            let image = process.image.ok_or(NOT_FOUND)?;
                            let root = toolbox.processes.native_exit_preflight(&owner, image)?;
                            Ok(Some((owner, image, process.info.thread, root)))
                        }).map_err(|cause| cause.status())??;
        if let Some((owner, image, thread, root)) = candidate
        {
            let runtime = crate::toolbox::native_threads::runtime_admission(&root, &owner)
                .map_err(|cause| cause.status())?;
            prepared.push((owner, image, thread, runtime));
        }
    }
    for (owner, image, thread, runtime) in prepared
    {
        let first = crate::kernel::with_toolbox(|toolbox| {
                        toolbox.processes.queue_native_failure(&owner,
                                                               image,
                                                               thread,
                                                               Outcome::Interrupted,
                                                               None)
                    }).map_err(|cause| cause.status())??;
        if let Err(cause) = scheduler::application::fail_owner(&runtime)
        {
            if first
            {
                let refused = crate::kernel::with_toolbox(|toolbox| {
                                  toolbox.processes.rollback_native_failure(&owner,
                                                                            thread,
                                                                            Outcome::Interrupted)
                              }).map_err(|cause| cause.status())??;
                drop(refused);
            }
            return Err(cause);
        }
        drop(runtime);
        crate::recovery::owner_failed(&owner);
    }
    progress();
    Ok(())
}
