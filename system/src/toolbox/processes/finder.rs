//! System-owned, staged shell replacement. Preparation never destroys the
//! report; replacement never reuses a dying owner's image or stack.
#![forbid(unsafe_code)]
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use mac_abi::*;
use mac_object_table::shared::InvocationGate as _;
use spinning_top::Spinlock;

use super::{ResourceBinding, RetirementCheckError};
use crate::{machine::scheduler::{self, PreparedThread},
            toolbox::{handles::InvocationGate,
                      runtime::{FinderAdmission, FinderPreparation}}};

/// Unpublished backing is still owned here; published backing remains owned by
/// its canonical THRD row. The admission authenticates both retry stages.
enum PendingThread
{
    Unpublished(PreparedThread),
    Published(u64)
}

enum ImageFailure
{
    Write(crate::images::RecoveryImageFailure),
    Retirement(RetirementCheckError),
    Service
    {
        stage:  &'static str,
        status: Status
    }
}
impl ImageFailure
{
    fn status(&self) -> Status
    {
        match self
        {
            Self::Write(cause) => cause.status(),
            Self::Retirement(_) => BAD_ABI,
            Self::Service { status,
                            .. } => *status
        }
    }

    fn retryable(&self) -> bool
    {
        matches!(self, Self::Write(cause) if cause.disposition == crate::images::RecoveryDisposition::Retryable)
    }
}
impl core::fmt::Display for ImageFailure
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Write(cause) => cause.fmt(out),
            Self::Retirement(cause) => cause.fmt(out),
            Self::Service { stage,
                            status } =>
            {
                write!(out,
                       "Finder {stage} refused with status {status}; original pending image and \
                        report retained")
            }
        }
    }
}

enum Work
{
    Requested
    {
        old:      ResourceBinding,
        // Unpublished backing retires before its original recovery scope pins.
        prepared: Option<FinderPreparation>,
        permit:   crate::allocator::recovery::Permit,
        activity: crate::recovery::Activity
    },
    Heap
    {
        old:       ResourceBinding,
        admission: FinderAdmission,
        heap:      Option<crate::toolbox::application_heap::RootBinding>
    },
    Tls
    {
        old:       ResourceBinding,
        admission: FinderAdmission,
        tls:       Option<crate::toolbox::native_threads::RootBinding>
    },
    Image
    {
        old: ResourceBinding, admission: FinderAdmission
    },
    Ready
    {
        old:          ResourceBinding,
        admission:    FinderAdmission,
        job:          crate::drivers::OwnedJob,
        failure:      Option<Status>,
        cancellation: Option<Status>
    },
    Directory
    {
        old:       ResourceBinding,
        admission: FinderAdmission,
        binding:   Option<crate::toolbox::files::local::directory::LaunchDirectory>
    },
    Copy
    {
        old: ResourceBinding, admission: FinderAdmission
    },
    Prepare
    {
        old:       ResourceBinding,
        admission: FinderAdmission,
        pending:   crate::images::PendingRecoveryImage
    },
    Commit
    {
        old:       ResourceBinding,
        next:      ResourceBinding,
        admission: FinderAdmission,
        pending:   crate::images::PendingRecoveryImage
    },
    RetireOld
    {
        old:       ResourceBinding,
        next:      ResourceBinding,
        admission: FinderAdmission,
        pending:   crate::images::PendingRecoveryImage
    },
    ImageWrite
    {
        next:      ResourceBinding,
        admission: FinderAdmission,
        pending:   crate::images::PendingRecoveryImage
    },
    ImagePaused
    {
        next:      ResourceBinding,
        admission: FinderAdmission,
        pending:   crate::images::PendingRecoveryImage,
        failure:   ImageFailure,
        attempted: bool
    },
    ImageTerminal
    {
        next:          ResourceBinding,
        admission:     FinderAdmission,
        pending:       crate::images::PendingRecoveryImage,
        failure:       ImageFailure,
        attempted:     bool,
        restart:       bool,
        power_failure: Option<(Status, u32)>
    },
    Wait
    {
        next:      ResourceBinding,
        admission: FinderAdmission,
        loaded:    crate::kernel::LoadedImage
    },
    Publish
    {
        next:      ResourceBinding,
        admission: FinderAdmission,
        prepared:  PreparedThread
    },
    Started
    {
        next: ResourceBinding, admission: FinderAdmission, thread: u64
    },
    RunningUi
    {
        next: ResourceBinding, admission: FinderAdmission, last: Status
    },
    Failure
    {
        target:   ResourceBinding,
        status:   Status,
        permit:   crate::allocator::recovery::Permit,
        activity: crate::recovery::Activity
    },
    BackingPaused
    {
        next:      ResourceBinding,
        admission: FinderAdmission,
        loaded:    crate::kernel::LoadedImage,
        status:    Status,
        attempted: bool
    },
    Terminal
    {
        next:          ResourceBinding,
        admission:     FinderAdmission,
        prepared:      PreparedThread,
        status:        Status,
        attempted:     bool,
        restart:       bool,
        power_failure: Option<(Status, u32)>
    },
    Paused
    {
        next:      ResourceBinding,
        admission: FinderAdmission,
        thread:    PendingThread,
        status:    Status,
        attempted: bool
    }
}
static FLIGHT: Spinlock<Option<Work>> = Spinlock::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static ORIGIN_OWNER: AtomicU64 = AtomicU64::new(0);
static ORIGIN_IMAGE: AtomicU64 = AtomicU64::new(0);
static WAIT_JOB: AtomicU64 = AtomicU64::new(0);
static WAKE: AtomicBool = AtomicBool::new(false);
static WAITING_FOR_STACK: AtomicBool = AtomicBool::new(false);
static WAITING_FOR_OLD: AtomicBool = AtomicBool::new(false);
static WAITING_FOR_REPORT: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);
static RETRY: AtomicBool = AtomicBool::new(false);
static UI_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Called only by the System-owned report action with its directory capability.
pub(crate) fn request(binding: ResourceBinding, permit: crate::allocator::recovery::Permit)
                      -> Result<(), Status>
{
    let activity = crate::context::recovery_activity().ok_or(NOT_SUPPORTED)?;
    if binding.owner() != crate::context::finder_app().0
       || binding.owner() != activity.owner()
       || !binding.ledger().accepting()
    {
        return Err(ACCESS_DENIED);
    }
    let image = binding.image().ok_or(BAD_ABI)?.id();
    InvocationGate::run(|| {
        let mut flight = FLIGHT.lock();
        if ACTIVE.load(Ordering::Acquire)
        {
            return if ORIGIN_OWNER.load(Ordering::Acquire) == binding.owner()
                      && ORIGIN_IMAGE.load(Ordering::Acquire) == image
            {
                Ok(())
            }
            else
            {
                Err(SERVICE_BUSY)
            };
        }
        ORIGIN_OWNER.store(binding.owner(), Ordering::Release);
        ORIGIN_IMAGE.store(image, Ordering::Release);
        ACTIVE.store(true, Ordering::Release);
        WAITING_FOR_REPORT.store(false, Ordering::Release);
        WAITING_FOR_OLD.store(false, Ordering::Release);
        UI_PROGRESS.store(false, Ordering::Release);
        *flight = Some(Work::Requested { old: binding,
                                         permit,
                                         activity,
                                         prepared: None });
        WAKE.store(true, Ordering::Release);
        Ok(())
    })
}

/// Retry changes only the work which already retains this genuine provisional
/// owner/image admission. Report origin metadata cannot create a new restart.
pub(crate) fn retry(binding: &ResourceBinding) -> Result<(), Status>
{
    InvocationGate::run(|| {
        let flight = FLIGHT.lock();
        if let Some(Work::Ready { old,
                                  failure: Some(_),
                                  .. }) = flight.as_ref()
        {
            if old.ledger().owner() != binding.ledger().owner()
               || old.image().map(crate::images::Binding::id)
                  != binding.image().map(crate::images::Binding::id)
            {
                return Err(ACCESS_DENIED);
            }
            RETRY.store(true, Ordering::Release);
            WAKE.store(true, Ordering::Release);
            return Ok(());
        }
        if matches!(flight.as_ref(), Some(Work::Terminal { .. }))
        {
            return Err(NOT_SUPPORTED);
        }
        let Some(Work::Paused { next,
                                admission,
                                .. }
                 | Work::BackingPaused { next,
                                       admission,
                                       .. }) = flight.as_ref()
        else
        {
            return Err(if RUNNING.load(Ordering::Acquire) { SERVICE_BUSY } else { NOT_FOUND });
        };
        if next.ledger().owner() != binding.ledger().owner()
           || admission.owner().owner() != binding.ledger().owner()
           || next.image().map(crate::images::Binding::id)
              != binding.image().map(crate::images::Binding::id)
        {
            return Err(ACCESS_DENIED);
        }
        RETRY.store(true, Ordering::Release);
        WAKE.store(true, Ordering::Release);
        Ok(())
    })
}

/// Minted only by actual System report input. Session/beneficiary proofs are
/// checked while Work is borrowed; no callback or UI operation crosses FLIGHT.
pub(crate) fn system_report_action(action: &crate::toolbox::runtime::SystemReportAction)
                                   -> Result<(), Status>
{
    use crate::toolbox::runtime::SystemReportActionKind;
    InvocationGate::run(|| {
        let mut flight = FLIGHT.lock();
        let Some(work) = flight.as_mut()
        else
        {
            return Err(if RUNNING.load(Ordering::Acquire) { SERVICE_BUSY } else { NOT_FOUND });
        };
        match action.kind()
        {
            SystemReportActionKind::Retry =>
            {
                let (next, admission) = match work
                {
                    Work::ImagePaused { next,
                                        admission,
                                        .. }
                    | Work::Paused { next,
                                   admission,
                                   .. }
                    | Work::BackingPaused { next,
                                          admission,
                                          .. } => (next, admission),
                    _ => return Err(SERVICE_BUSY)
                };
                let report = admission.system_report().ok_or(NOT_FOUND)?;
                if !action.authorizes(report)
                   || !action.beneficiary_matches(next)
                   || next.ledger().owner() != admission.owner().owner()
                   || !admission.owner().accepting()
                {
                    return Err(ACCESS_DENIED);
                }
                RETRY.store(true, Ordering::Release);
            }
            SystemReportActionKind::Restart =>
            {
                let (admission, restart) = match work
                {
                    Work::ImageTerminal { admission,
                                          restart,
                                          .. }
                    | Work::Terminal { admission,
                                     restart,
                                     .. } => (admission, restart),
                    _ => return Err(SERVICE_BUSY)
                };
                let report = admission.system_report().ok_or(NOT_FOUND)?;
                if !action.authorizes(report)
                {
                    return Err(ACCESS_DENIED);
                }
                // System owns this report independently. A stopped beneficiary
                // cannot revoke the real System Restart action.
                *restart = true;
            }
        }
        WAKE.store(true, Ordering::Release);
        Ok(())
    })
}

/// Called only from the System-owned terminal Finder report action.
/// The real retained admission authorizes this request; no owner label creates it.
pub(crate) fn restart_system(binding: &ResourceBinding) -> Result<(), Status>
{
    InvocationGate::run(|| {
        let mut flight = FLIGHT.lock();
        let Some(Work::Terminal { next,
                                  admission,
                                  restart,
                                  .. }) = flight.as_mut()
        else
        {
            return Err(NOT_FOUND);
        };
        if next.ledger().owner() != binding.ledger().owner()
           || admission.owner().owner() != binding.ledger().owner()
           || next.image().map(crate::images::Binding::id)
              != binding.image().map(crate::images::Binding::id)
           || !admission.owner().accepting()
        {
            return Err(ACCESS_DENIED);
        }
        *restart = true;
        WAKE.store(true, Ordering::Release);
        Ok(())
    })
}

/// A real completion may release memory needed by a retained warning or
/// post-start control refresh. Keep the callback bounded and allocation-free.
pub(crate) fn cleanup_progress()
{
    if ACTIVE.load(Ordering::Acquire)
    {
        UI_PROGRESS.store(true, Ordering::Release);
        WAKE.store(true, Ordering::Release);
    }
}

struct Running;
impl Drop for Running
{
    fn drop(&mut self)
    {
        RUNNING.store(false, Ordering::Release);
    }
}

fn desktop<R>(binding: &ResourceBinding,
              call: impl FnOnce(&mut crate::toolbox::runtime::Desktop) -> Result<R, Status>)
              -> Result<R, Status>
{
    crate::kernel::with_desktop(|desktop| {
        desktop.enter(binding.owner());
        crate::kernel::with_process_resources(binding, || call(desktop))?
    }).map_err(|error| error.status())?
}

/// Desktop ownership contention has no completion event of its own. Retain
/// the exact work and try again on the next maintenance turn; service refusals
/// remain event-driven. Classify the actual access cause before trap conversion.
fn report_access_status(cause: crate::state::AccessError) -> Status
{
    if cause == crate::state::AccessError::Busy
    {
        UI_PROGRESS.store(true, Ordering::Release);
        WAKE.store(true, Ordering::Release);
    }
    cause.status()
}

fn report_ui<R>(admission: &FinderAdmission,
                call: impl FnOnce(&mut crate::toolbox::runtime::Desktop) -> Result<R, Status>)
                -> Result<R, Status>
{
    let report = admission.system_report().ok_or(BAD_ABI)?;
    crate::kernel::with_desktop(|desktop| desktop.with_system_report(report, call))
        .map_err(report_access_status)??
}

fn handoff_report(admission: &mut FinderAdmission) -> Result<(), Status>
{
    crate::kernel::with_desktop(|desktop| admission.handoff_system_report(desktop))
        .map_err(report_access_status)?
}

/// A real same-session close owns presentation now. Hand the original lease
/// to that service without trying to reopen its acknowledged Window. No numeric
/// refusal or absent ID is accepted as proof of closing.
fn handoff_closing_report(admission: &mut FinderAdmission) -> Result<bool, Status>
{
    crate::kernel::with_desktop(|desktop| {
        let report = admission.system_report().ok_or(BAD_ABI)?;
        if !desktop.system_report_closing(report)?
        {
            return Ok(false);
        }
        admission.handoff_system_report(desktop)?;
        Ok(true)
    }).map_err(report_access_status)?
}

fn notify_image_failure(admission: &FinderAdmission, failure: &ImageFailure, terminal: bool,
                        power_failure: Option<(Status, u32)>)
                        -> Result<(), Status>
{
    let detail = crate::toolbox::crashes::FailureDetail::prepare(failure)?;
    report_ui(admission, |desktop| {
        desktop.finder_image_failure(failure.status(), terminal, detail, power_failure)
    })
}

fn cancel_with_report(admission: &mut FinderAdmission, status: Status) -> bool
{
    match handoff_closing_report(admission)
    {
        Ok(true) => return true,
        Ok(false) => (),
        Err(_) => return false
    }
    let failure = ImageFailure::Service { stage: "replacement cancellation; its actual \
                                                  resources are retiring",
                                          status };
    if notify_image_failure(admission, &failure, true, None).is_ok()
    {
        handoff_report(admission).is_ok()
    }
    else
    {
        // BEGIN or dismissal may have requested closing during presentation.
        // Recheck the same authentic service, never infer it from a Status.
        matches!(handoff_closing_report(admission), Ok(true))
    }
}

fn refresh_started_report(admission: &FinderAdmission) -> Result<(), Status>
{
    crate::kernel::with_desktop(|desktop| desktop.finish_finder_start_ui(admission))
        .map_err(report_access_status)?
}

fn image_failure(next: ResourceBinding, admission: FinderAdmission,
                 pending: crate::images::PendingRecoveryImage, failure: ImageFailure)
                 -> Work
{
    serial!("FINDER_IMAGE_PUBLICATION_REFUSED app={} {failure}", admission.id());
    if failure.retryable()
    {
        Work::ImagePaused { next,
                            admission,
                            pending,
                            failure,
                            attempted: false }
    }
    else
    {
        Work::ImageTerminal { next,
                              admission,
                              pending,
                              failure,
                              attempted: false,
                              restart: false,
                              power_failure: None }
    }
}

/// One actual provider/flush/restart path, outside Desktop and FLIGHT. Both
/// controller-owned and standalone System report actions use this operation.
pub(crate) fn perform_system_report_restart() -> Option<(Status, u32)>
{
    let mut call = mac_abi::power::PowerCall { operation: mac_abi::power::RESTART,
                                               ..Default::default() };
    match crate::toolbox::power::native(&mut call)
    {
        Ok(()) => None,
        Err(cause) => Some((cause, call.failure_stage))
    }
}

fn capacity_status(cause: crate::toolbox::runtime::FinderCapacityError) -> Status
{
    let status = cause.status();
    // Capacity contention is not a fault or a reason to write UART on every
    // retry. A terminal preparation refusal retains useful stage/cause detail.
    if status != SERVICE_BUSY
    {
        serial!("FINDER_CAPACITY_REFUSED: {cause}");
    }
    status
}

fn reserve_finder(binding: &ResourceBinding, permit: &crate::allocator::recovery::Permit,
                  prepared: &mut Option<FinderPreparation>)
                  -> Result<FinderAdmission, Status>
{
    let plan = desktop(binding, |desktop| desktop.plan_finder_capacity().map_err(capacity_status))?;
    // The real recovery allocation scope remains active, but no Desktop,
    // directory or owner lock crosses these fresh shared-buffer allocations.
    if prepared.is_none()
    {
        *prepared = Some(plan.prepare_recipient()?);
    }
    let mut capacity = plan.prepare().map_err(capacity_status)?;
    let result = desktop(binding, |desktop| {
        desktop.reserve_finder(binding.owner(), permit.clone(), &mut capacity, prepared)
    });
    // Successful installation leaves obsolete empty arrays here. On refusal
    // these are still the original prepared arrays. Neither drops under Desktop.
    drop(capacity);
    if let Some(prepared) = prepared.as_mut()
    {
        prepared.release_report();
    }
    result
}

fn step(work: Work) -> Option<Work>
{
    let (permit, activity) = match &work
    {
        Work::Requested { permit,
                          activity,
                          .. }
        | Work::Failure { permit,
                        activity,
                        .. } => (permit.clone(), activity.clone()),
        Work::Heap { admission,
                     .. }
        | Work::Tls { admission,
                    .. }
        | Work::Image { admission,
                      .. }
        | Work::Ready { admission,
                      .. }
        | Work::Directory { admission,
                          .. }
        | Work::Copy { admission,
                     .. }
        | Work::Prepare { admission,
                        .. }
        | Work::Commit { admission,
                       .. }
        | Work::RetireOld { admission,
                          .. }
        | Work::ImageWrite { admission,
                           .. }
        | Work::ImagePaused { admission,
                            .. }
        | Work::ImageTerminal { admission,
                              .. }
        | Work::Wait { admission,
                     .. }
        | Work::Publish { admission,
                        .. }
        | Work::Started { admission,
                        .. }
        | Work::RunningUi { admission,
                          .. }
        | Work::BackingPaused { admission,
                              .. }
        | Work::Terminal { admission,
                         .. }
        | Work::Paused { admission,
                       .. } => (admission.permit(), admission.activity())
    };
    // The protocol root remains the original modal root through preparation
    // and disposal. Keep the work if a reclaimed context refuses scope entry.
    let mut retained = Some(work);
    crate::context::with_recovery_allocation(permit, || {
        activity.run(|| retained.take().and_then(step_inner)).unwrap_or_else(|_| retained)
    })
}
fn step_inner(work: Work) -> Option<Work>
{
    match work
    {
        Work::Requested { old,
                          permit,
                          activity,
                          mut prepared } =>
        {
            match reserve_finder(&old, &permit, &mut prepared)
            {
                Ok(admission) =>
                {
                    Some(Work::Heap { old,
                                      admission,
                                      heap: None })
                }
                Err(SERVICE_BUSY) =>
                {
                    Some(Work::Requested { old,
                                           permit,
                                           activity,
                                           prepared })
                }
                Err(status) =>
                {
                    Some(Work::Failure { target: old,
                                         status,
                                         permit,
                                         activity })
                }
            }
        }
        Work::Heap { old,
                     admission,
                     heap } =>
        {
            if !admission.owner().accepting()
            {
                return Some(Work::Failure { target:   old,
                                            status:   SERVICE_BUSY,
                                            permit:   admission.permit(),
                                            activity: admission.activity() });
            }
            // This stage runs after Desktop has released the real provisional
            // directory entry. The trusted recovery activity/permit remains
            // scoped by step(); the account is the NEW Finder's genuine one.
            let heap = match heap
            {
                Some(heap) => heap,
                None =>
                {
                    let preparation =
                        crate::toolbox::application_heap::prepare_root(admission.owner().clone(),
                                                                       admission.allocation());
                    let published = match preparation
                    {
                        Ok(prepared) => crate::toolbox::application_heap::publish_root(prepared)
                            .map_err(|failure| {
                                let status = failure.cause.status();
                                serial!("FINDER_HEAP_PUBLICATION_REFUSED app={} {}",
                                        admission.id(), failure.cause);
                                failure.prepared.dispose();
                                status
                            }),
                        Err(cause) =>
                        {
                            serial!("FINDER_HEAP_PREPARATION_REFUSED app={} {cause}", admission.id());
                            Err(cause.status())
                        }
                    };
                    match published
                    {
                        Ok(heap) => heap,
                        Err(SERVICE_BUSY) =>
                        {
                            return Some(Work::Heap { old,
                                                     admission,
                                                     heap: None });
                        }
                        Err(status) =>
                        {
                            return Some(Work::Failure { target: old,
                                                        status,
                                                        permit: admission.permit(),
                                                        activity: admission.activity() });
                        }
                    }
                }
            };
            match desktop(&old, |desktop| {
                      desktop.processes.bind_heap(admission.owner(), heap.clone())
                  })
            {
                Ok(()) =>
                {
                    Some(Work::Tls { old,
                                     admission,
                                     tls: None })
                }
                Err(SERVICE_BUSY) =>
                {
                    Some(Work::Heap { old,
                                      admission,
                                      heap: Some(heap) })
                }
                Err(status) =>
                {
                    Some(Work::Failure { target: old,
                                         status,
                                         permit: admission.permit(),
                                         activity: admission.activity() })
                }
            }
        }
        Work::Tls { old,
                    admission,
                    tls } =>
        {
            if !admission.owner().accepting()
            {
                return Some(Work::Failure { target:   old,
                                            status:   SERVICE_BUSY,
                                            permit:   admission.permit(),
                                            activity: admission.activity() });
            }
            let tls = match tls
            {
                Some(tls) => tls,
                None =>
                {
                    let prepared =
                        crate::toolbox::native_threads::prepare_root(admission.owner().clone(),
                                                                     admission.allocation());
                    let result = prepared.and_then(|prepared| {
                        crate::toolbox::native_threads::publish_root(prepared).map_err(|failure| {
                            let cause = failure.cause; failure.prepared.dispose(); cause
                        })
                    });
                    match result
                    {
                        Ok(tls) => tls,
                        Err(cause) =>
                        {
                            return Some(Work::Failure { target:   old,
                                                        status:   cause.status(),
                                                        permit:   admission.permit(),
                                                        activity: admission.activity() });
                        }
                    }
                }
            };
            match desktop(&old, |desktop| {
                      desktop.processes.bind_tls(admission.owner(), tls.clone())
                  })
            {
                Ok(()) =>
                {
                    Some(Work::Image { old,
                                       admission })
                }
                Err(SERVICE_BUSY) =>
                {
                    Some(Work::Tls { old,
                                     admission,
                                     tls: Some(tls) })
                }
                Err(status) =>
                {
                    Some(Work::Failure { target: old,
                                         status,
                                         permit: admission.permit(),
                                         activity: admission.activity() })
                }
            }
        }
        Work::Image { old,
                      admission } =>
        {
            let selected = (|| {
                let tls = desktop(&old, |desktop| desktop.processes.tls(admission.id()))?;
                let provider = crate::kernel::select_application_services(admission.owner(),
                                                                          admission.allocation(),
                                                                          &tls,
                                                                          admission.original())?;
                provider.map(|provider| super::launch::ready_job(&tls, admission.owner(), provider))
                        .transpose()
            })();
            match selected
            {
                Ok(Some(job)) =>
                {
                    WAIT_JOB.store(job.notification_id(), Ordering::Release);
                    WAKE.store(true, Ordering::Release);
                    Some(Work::Ready { old,
                                       admission,
                                       job,
                                       failure: None,
                                       cancellation: None })
                }
                Ok(None) =>
                {
                    Some(Work::Directory { old,
                                           admission,
                                           binding: None })
                }
                Err(SERVICE_BUSY) =>
                {
                    Some(Work::Image { old,
                                       admission })
                }
                Err(status) =>
                {
                    Some(Work::Failure { target: old,
                                         status,
                                         permit: admission.permit(),
                                         activity: admission.activity() })
                }
            }
        }
        Work::Ready { old,
                      admission,
                      mut job,
                      mut failure,
                      mut cancellation } =>
        {
            // No coordinator, Desktop or registry guard crosses this callback.
            // Admission retains the genuine recovery Activity until terminal ACK.
            let result = job.run_once();
            match result
            {
                Ok(Some(completed)) =>
                {
                    WAIT_JOB.store(0, Ordering::Release);
                    let status = failure.or_else(|| {
                                     match completed.result
                                     {
                                         Ok(bytes) if bytes.is_empty() => None,
                                         Ok(_) => Some(BAD_ABI),
                                         Err(crate::drivers::jobs::Failure::Cancelled) =>
                                         {
                                             Some(OPERATION_CANCELLED)
                                         }
                                         Err(crate::drivers::jobs::Failure::Service(status)) =>
                                         {
                                             Some(status)
                                         }
                                     }
                                 });
                    return Some(match status
                                {
                                    None =>
                                    {
                                        Work::Directory { old,
                                                          admission,
                                                          binding: None }
                                    }
                                    Some(status) =>
                                    {
                                        Work::Failure { target: old,
                                                        status,
                                                        permit: admission.permit(),
                                                        activity: admission.activity() }
                                    }
                                });
                }
                Ok(None) => (),
                Err(status) =>
                {
                    if failure.is_none() || RETRY.swap(false, Ordering::AcqRel)
                    {
                        failure.get_or_insert(status);
                        InvocationGate::run(|| {
                            admission.owner().stop_admission();
                            scheduler::retire_owner(admission.id());
                        });
                        super::retirement::resources_released();
                        match job.cancel()
                        {
                            Ok(()) => WAKE.store(true, Ordering::Release),
                            Err(cause) =>
                            {
                                cancellation = Some(cause);
                                serial!("FINDER_READY_CANCEL_RETAINED app={} original={status} \
                                         cancellation={cause}; owned job/runtime retained",
                                        admission.id());
                                let _notification =
                                    desktop(&old, |desktop| desktop.finder_restart_failed(cause));
                            }
                        }
                    }
                    else if cancellation != Some(status)
                    {
                        cancellation = Some(status);
                        serial!("FINDER_READY_DELIVERY_RETAINED app={} original={failure:?} \
                                 delivery={status}; owned job/runtime retained",
                                admission.id());
                        let _notification =
                            desktop(&old, |desktop| desktop.finder_restart_failed(status));
                    }
                }
            }
            Some(Work::Ready { old,
                               admission,
                               job,
                               failure,
                               cancellation })
        }
        Work::Directory { old,
                          admission,
                          mut binding } =>
        {
            let result = (|| {
                if binding.is_none()
                {
                    let directory = crate::toolbox::files::local::directory::LaunchDirectory::prepare(
                        admission.owner(), admission.allocation())?;
                    WAIT_JOB.store(directory.notification_id().unwrap_or(0), Ordering::Release);
                    binding = Some(directory);
                }
                let completed = crate::toolbox::files::local::directory::LaunchDirectory::advance(
                    &mut binding)?;
                if !completed && WAIT_JOB.load(Ordering::Acquire) == 0
                {
                    return Err(BAD_ABI);
                }
                Ok(completed)
            })();
            match result
            {
                Ok(false) =>
                {
                    Some(Work::Directory { old,
                                           admission,
                                           binding })
                }
                Ok(true) =>
                {
                    WAIT_JOB.store(0, Ordering::Release);
                    let original = admission.original_catalog();
                    serial!("PROCESS_DIRECTORY_READY app={} policy=Documents phase=replacement",
                            admission.id());
                    if let Err(status) = super::pin_launch_executable(admission.owner(),
                                                                      admission.allocation(),
                                                                      original)
                    {
                        serial!("FINDER_EXECUTABLE_REFUSED app={} volume={} id={} status={status}",
                                admission.id(),
                                original.volume,
                                original.id);
                        return Some(Work::Failure { target: old,
                                                    status,
                                                    permit: admission.permit(),
                                                    activity: admission.activity() });
                    }
                    serial!("PROCESS_EXECUTABLE_READY app={} volume={} id={} phase=replacement",
                            admission.id(),
                            original.volume,
                            original.id);
                    Some(Work::Copy { old,
                                      admission })
                }
                Err(status) =>
                {
                    WAIT_JOB.store(0, Ordering::Release);
                    serial!("FINDER_DIRECTORY_REFUSED app={} policy=Documents status={status}",
                            admission.id());
                    Some(Work::Failure { target: old,
                                         status,
                                         permit: admission.permit(),
                                         activity: admission.activity() })
                }
            }
        }
        Work::Copy { old,
                     admission } =>
        {
            let target =
                crate::allocator::Target::Owner { account: admission.allocation(),
                                                  class:
                                                      mac_kernel_model::policy::MemoryClass::Kernel };
            match crate::allocator::with_target(target, || {
                      crate::kernel::prepare_recovery_application(admission.owner(),
                                                                  &admission.allocation(),
                                                                  admission.original(),
                                                                  admission.permit())
                  })
            {
                Ok(pending) =>
                {
                    Some(Work::Prepare { old,
                                         admission,
                                         pending })
                }
                Err(SERVICE_BUSY) =>
                {
                    Some(Work::Copy { old,
                                      admission })
                }
                Err(status) =>
                {
                    Some(Work::Failure { target: old,
                                         status,
                                         permit: admission.permit(),
                                         activity: admission.activity() })
                }
            }
        }
        Work::Prepare { old,
                        admission,
                        pending } =>
        {
            match desktop(&old, |desktop| {
                      desktop.bind_finder_image(&admission, pending.binding())?;
                      desktop.processes.resource_binding(admission.id())
                  })
            {
                Ok(next) =>
                {
                    Some(Work::Commit { old,
                                        next,
                                        admission,
                                        pending })
                }
                Err(SERVICE_BUSY) =>
                {
                    Some(Work::Prepare { old,
                                         admission,
                                         pending })
                }
                Err(status) =>
                {
                    Some(Work::Failure { target: old,
                                         status,
                                         permit: admission.permit(),
                                         activity: admission.activity() })
                }
            }
        }
        Work::Commit { old,
                       next,
                       mut admission,
                       pending } =>
        {
            // Detach the full report into genuine System ownership before
            // changing the Finder role or stopping any original owner.
            if admission.system_report().is_none()
            {
                let adoption = desktop(&old, |desktop| {
                    desktop.adopt_finder_system_report(&old, &mut admission)
                });
                if let Err(status) = adoption
                {
                    if admission.system_report().is_some()
                    {
                        return Some(image_failure(next,
                                                  admission,
                                                  pending,
                                                  ImageFailure::Service { stage:
                                                                              "System report \
                                                                               handoff",
                                                                          status }));
                    }
                    return Some(if status == SERVICE_BUSY
                                {
                                    Work::Commit { old,
                                                   next,
                                                   admission,
                                                   pending }
                                }
                                else
                                {
                                    Work::Failure { target: old,
                                                    status,
                                                    permit: admission.permit(),
                                                    activity: admission.activity() }
                                });
                }
            }
            match desktop(&old, |desktop| desktop.commit_finder_role(&admission))
            {
                Ok(()) =>
                {
                    WAITING_FOR_REPORT.store(false, Ordering::Release);
                    Some(Work::RetireOld { old,
                                           next,
                                           admission,
                                           pending })
                }
                Err(status) =>
                {
                    Some(image_failure(next,
                                       admission,
                                       pending,
                                       ImageFailure::Service { stage: "role handoff after \
                                                                       System report retention",
                                                               status }))
                }
            }
        }
        Work::RetireOld { old,
                          next,
                          admission,
                          pending } =>
        {
            if !admission.owner().accepting()
            {
                WAITING_FOR_OLD.store(false, Ordering::Release);
                return Some(image_failure(next,
                                          admission,
                                          pending,
                                          ImageFailure::Service { stage:  "replacement stopped \
                                                                           while waiting for old \
                                                                           Process removal",
                                                                  status: OPERATION_CANCELLED }));
            }
            // Arm first. Removal before the query is seen as absence; removal
            // afterward posts the genuine per-change wake. Never clear that
            // wake after inspecting a still-present old directory entry.
            WAITING_FOR_OLD.store(true, Ordering::Release);
            let removed =
                crate::kernel::with_desktop(|desktop| desktop.processes.retired_binding(&old));
            match removed
            {
                Ok(Ok(true)) =>
                {
                    WAITING_FOR_OLD.store(false, Ordering::Release);
                    Some(Work::ImageWrite { next,
                                            admission,
                                            pending })
                }
                Ok(Ok(false)) =>
                {
                    Some(Work::RetireOld { old,
                                           next,
                                           admission,
                                           pending })
                }
                Err(crate::state::AccessError::Busy) =>
                {
                    // No directory observation was made. Do not sleep on an
                    // unrelated Desktop holder without a real removal wake.
                    WAITING_FOR_OLD.store(false, Ordering::Release);
                    Some(Work::RetireOld { old,
                                           next,
                                           admission,
                                           pending })
                }
                Ok(Err(cause)) =>
                {
                    WAITING_FOR_OLD.store(false, Ordering::Release);
                    Some(image_failure(next, admission, pending, ImageFailure::Retirement(cause)))
                }
                Err(cause) =>
                {
                    WAITING_FOR_OLD.store(false, Ordering::Release);
                    Some(image_failure(next,
                                       admission,
                                       pending,
                                       ImageFailure::Service { stage:  "old Process retirement \
                                                                        observation",
                                                               status: cause.status() }))
                }
            }
        }
        Work::ImageWrite { next,
                           admission,
                           mut pending } =>
        {
            if let Err(cause) = pending.advance()
            {
                return Some(image_failure(next, admission, pending, ImageFailure::Write(cause)));
            }
            match pending.finish()
            {
                Ok(loaded) =>
                {
                    Some(Work::Wait { next,
                                      admission,
                                      loaded })
                }
                Err(cause) =>
                {
                    Some(image_failure(next,
                                       admission,
                                       cause.pending,
                                       ImageFailure::Write(cause.failure)))
                }
            }
        }
        Work::ImagePaused { next,
                            admission,
                            pending,
                            failure,
                            mut attempted } =>
        {
            if !admission.owner().accepting()
            {
                return Some(image_failure(next,
                                          admission,
                                          pending,
                                          ImageFailure::Service { stage:  "replacement stopped \
                                                                           while image \
                                                                           publication was paused",
                                                                  status: OPERATION_CANCELLED }));
            }
            if RETRY.swap(false, Ordering::AcqRel)
            {
                return Some(Work::ImageWrite { next,
                                               admission,
                                               pending });
            }
            if !attempted || UI_PROGRESS.swap(false, Ordering::AcqRel)
            {
                let _notification = notify_image_failure(&admission, &failure, false, None);
                attempted = true;
            }
            Some(Work::ImagePaused { next,
                                     admission,
                                     pending,
                                     failure,
                                     attempted })
        }
        Work::ImageTerminal { next,
                              mut admission,
                              pending,
                              failure,
                              mut attempted,
                              restart,
                              mut power_failure } =>
        {
            if !admission.owner().accepting()
               && matches!(handoff_closing_report(&mut admission), Ok(true))
            {
                return None;
            }
            if restart
            {
                power_failure = perform_system_report_restart();
                attempted = false;
            }
            if !attempted || UI_PROGRESS.swap(false, Ordering::AcqRel)
            {
                let shown = notify_image_failure(&admission, &failure, true, power_failure);
                attempted = true;
                if !admission.owner().accepting()
                   && (if shown.is_ok()
                   {
                       handoff_report(&mut admission).is_ok()
                   }
                   else
                   {
                       matches!(handoff_closing_report(&mut admission), Ok(true))
                   })
                {
                    // The independent System report now owns the full text,
                    // events and Restart. Actual pending/owner cleanup follows
                    // normally after leaving this Desktop/FLIGHT-free turn.
                    return None;
                }
            }
            Some(Work::ImageTerminal { next,
                                       admission,
                                       pending,
                                       failure,
                                       attempted,
                                       restart: false,
                                       power_failure })
        }
        Work::Wait { next,
                     admission,
                     loaded } =>
        {
            if !admission.owner().accepting()
            {
                WAITING_FOR_STACK.store(false, Ordering::Release);
                return Some(Work::BackingPaused { next,
                                                  admission,
                                                  loaded,
                                                  status: OPERATION_CANCELLED,
                                                  attempted: false });
            }
            WAITING_FOR_STACK.store(true, Ordering::Release);
            let loan = match scheduler::take_finder_recovery()
            {
                Ok(loan) => loan,
                Err(SERVICE_BUSY) =>
                {
                    return Some(Work::Wait { next,
                                             admission,
                                             loaded });
                }
                Err(status) =>
                {
                    return Some(Work::BackingPaused { next,
                                                      admission,
                                                      loaded,
                                                      status,
                                                      attempted: false });
                }
            };
            WAITING_FOR_STACK.store(false, Ordering::Release);
            let (charges, heap, tls) = match desktop(&next, |desktop| {
                      Ok((desktop.processes.thread_charges(admission.id())?,
                          desktop.processes.heap(admission.id())?,
                          desktop.processes.tls(admission.id())?))
                  })
            {
                Ok(admission) => admission,
                Err(status) =>
                {
                    return Some(Work::BackingPaused { next,
                                                      admission,
                                                      loaded,
                                                      status,
                                                      attempted: false });
                }
            };
            match scheduler::prepare_recovery(admission.owner().clone(),
                                              loaded,
                                              charges,
                                              loan,
                                              heap,
                                              tls)
            {
                Ok(prepared) =>
                {
                    Some(Work::Publish { next,
                                         admission,
                                         prepared })
                }
                Err(failure) =>
                {
                    let scheduler::RecoveryPrepareFailure { status,
                                                            loaded,
                                                            loan } = failure;
                    drop(loan);
                    Some(Work::BackingPaused { next,
                                               admission,
                                               loaded,
                                               status,
                                               attempted: false })
                }
            }
        }
        Work::Publish { next,
                        admission,
                        prepared } =>
        {
            match scheduler::spawn_prepared(prepared, false)
            {
                Ok(thread) =>
                {
                    Some(Work::Started { next,
                                         admission,
                                         thread })
                }
                Err(failure) =>
                {
                    match failure.disposition
                    {
                        scheduler::SpawnRefusal::RetryableAdmission =>
                        {
                            Some(Work::Paused { next,
                                                admission,
                                                thread:
                                                    PendingThread::Unpublished(failure.prepared),
                                                status: failure.status,
                                                attempted: false })
                        }
                        scheduler::SpawnRefusal::TerminalBinding =>
                        {
                            Some(Work::Terminal { next,
                                                  admission,
                                                  prepared: failure.prepared,
                                                  status: failure.status,
                                                  attempted: false,
                                                  restart: false,
                                                  power_failure: None })
                        }
                        // Stopping is irreversible. Dropping admission requests the real
                        // walk; a row already owned by that walk keeps its own backing.
                        scheduler::SpawnRefusal::Cancelled =>
                        {
                            let mut admission = admission;
                            if cancel_with_report(&mut admission, failure.status)
                            {
                                None
                            }
                            else
                            {
                                Some(Work::Paused { next,
                                                    admission,
                                                    thread:
                                                        PendingThread::Unpublished(failure.prepared),
                                                    status: failure.status,
                                                    attempted: true })
                            }
                        }
                    }
                }
            }
        }
        Work::Started { next,
                        mut admission,
                        thread } =>
        {
            // The inner result belongs only to post-commit presentation. It
            // cannot turn a runnable thread back into failed preparation.
            match desktop(&next, |desktop| {
                      desktop.prepare_finder_start(&admission, thread)?;
                      desktop.commit_finder_start(&mut admission, thread)?;
                      UI_PROGRESS.store(false, Ordering::Release);
                      Ok(desktop.finish_finder_start_ui(&admission))
                  })
            {
                Ok(Ok(())) =>
                {
                    match handoff_report(&mut admission)
                    {
                        Ok(()) => None,
                        Err(status) =>
                        {
                            Some(Work::RunningUi { next,
                                                   admission,
                                                   last: status })
                        }
                    }
                }
                Ok(Err(status)) =>
                {
                    serial!("FINDER_REPORT_REFRESH_RETAINED app={} status={status}; Finder \
                             remains runnable",
                            admission.id());
                    Some(Work::RunningUi { next,
                                           admission,
                                           last: status })
                }
                Err(status) =>
                {
                    Some(Work::Paused { next,
                                        admission,
                                        thread: PendingThread::Published(thread),
                                        status,
                                        attempted: false })
                }
            }
        }
        Work::RunningUi { next,
                          mut admission,
                          mut last } =>
        {
            if !admission.owner().accepting()
            {
                if cancel_with_report(&mut admission, OPERATION_CANCELLED)
                {
                    return None;
                }
                return Some(Work::RunningUi { next,
                                              admission,
                                              last: OPERATION_CANCELLED });
            }
            UI_PROGRESS.store(false, Ordering::Release);
            match refresh_started_report(&admission)
            {
                Ok(()) =>
                {
                    match handoff_report(&mut admission)
                    {
                        Ok(()) => None,
                        Err(status) =>
                        {
                            Some(Work::RunningUi { next,
                                                   admission,
                                                   last: status })
                        }
                    }
                }
                Err(status) =>
                {
                    if last != status
                    {
                        serial!("FINDER_REPORT_REFRESH_RETAINED app={} status={status}; Finder \
                                 remains runnable",
                                admission.id());
                        last = status;
                    }
                    Some(Work::RunningUi { next,
                                           admission,
                                           last })
                }
            }
        }
        Work::Failure { target,
                        status,
                        permit,
                        activity } =>
        {
            match desktop(&target, |desktop| desktop.finder_restart_failed(status))
            {
                Ok(()) => None,
                Err(_) =>
                {
                    Some(Work::Failure { target,
                                         status,
                                         permit,
                                         activity })
                }
            }
        }
        Work::BackingPaused { next,
                              mut admission,
                              loaded,
                              status,
                              mut attempted } =>
        {
            if !admission.owner().accepting()
            {
                if cancel_with_report(&mut admission, OPERATION_CANCELLED)
                {
                    return None;
                }
                return Some(Work::BackingPaused { next,
                                                  admission,
                                                  loaded,
                                                  status: OPERATION_CANCELLED,
                                                  attempted: true });
            }
            if RETRY.swap(false, Ordering::AcqRel)
            {
                return Some(Work::Wait { next,
                                         admission,
                                         loaded });
            }
            if !attempted || UI_PROGRESS.swap(false, Ordering::AcqRel)
            {
                let _notification =
                    report_ui(&admission, |desktop| desktop.finder_restart_failed(status));
                attempted = true;
            }
            Some(Work::BackingPaused { next,
                                       admission,
                                       loaded,
                                       status,
                                       attempted })
        }
        Work::Terminal { next,
                         mut admission,
                         prepared,
                         status,
                         mut attempted,
                         restart,
                         mut power_failure } =>
        {
            if !admission.owner().accepting()
            {
                if cancel_with_report(&mut admission, OPERATION_CANCELLED)
                {
                    return None;
                }
                return Some(Work::Terminal { next,
                                             admission,
                                             prepared,
                                             status: OPERATION_CANCELLED,
                                             attempted: true,
                                             restart: false,
                                             power_failure });
            }
            if restart
            {
                // No Desktop/FLIGHT guard crosses the actual provider/flush path.
                power_failure = perform_system_report_restart();
                attempted = false;
            }
            if !attempted || UI_PROGRESS.swap(false, Ordering::AcqRel)
            {
                let _notification = report_ui(&admission, |desktop| {
                    desktop.finder_restart_terminal(status, power_failure)
                });
                attempted = true;
            }
            Some(Work::Terminal { next,
                                  admission,
                                  prepared,
                                  status,
                                  attempted,
                                  restart: false,
                                  power_failure })
        }
        Work::Paused { next,
                       mut admission,
                       thread,
                       status,
                       mut attempted } =>
        {
            if !admission.owner().accepting()
            {
                if cancel_with_report(&mut admission, OPERATION_CANCELLED)
                {
                    return None;
                }
                return Some(Work::Paused { next,
                                           admission,
                                           thread,
                                           status: OPERATION_CANCELLED,
                                           attempted: true });
            }
            if RETRY.swap(false, Ordering::AcqRel)
            {
                return Some(match thread
                            {
                                PendingThread::Unpublished(prepared) =>
                                {
                                    Work::Publish { next,
                                                    admission,
                                                    prepared }
                                }
                                PendingThread::Published(thread) =>
                                {
                                    Work::Started { next,
                                                    admission,
                                                    thread }
                                }
                            });
            }
            if !attempted || UI_PROGRESS.swap(false, Ordering::AcqRel)
            {
                let _notification =
                    report_ui(&admission, |desktop| desktop.finder_restart_failed(status));
                attempted = true;
            }
            Some(Work::Paused { next,
                                admission,
                                thread,
                                status,
                                attempted })
        }
    }
}

/// At most one preparation/checkpoint per maintenance sample, after input.
/// No coordinator lock is held across a manager, loader or cleanup callback.
pub(crate) fn pump()
{
    if !ACTIVE.load(Ordering::Acquire)
       || !WAKE.load(Ordering::Acquire)
       || RUNNING.swap(true, Ordering::AcqRel)
    {
        return;
    }
    let _running = Running;
    let work = InvocationGate::run(|| FLIGHT.lock().take());
    let Some(work) = work
    else
    {
        return;
    };
    WAKE.store(false, Ordering::Release);
    let next = step(work);
    let sleep = matches!(&next,
                         Some(Work::Terminal { attempted: true,
                                               .. }
                              | Work::ImageTerminal { attempted: true,
                                                      .. }
                              | Work::ImagePaused { attempted: true,
                                                    .. }
                              | Work::Ready { .. }
                              | Work::Directory { binding: Some(_),
                                                  .. }
                              | Work::Paused { attempted: true,
                                               .. }
                              | Work::BackingPaused { attempted: true,
                                                      .. }
                              | Work::RunningUi { .. }))
                || (matches!(&next, Some(Work::Commit { .. }))
                    && WAITING_FOR_REPORT.load(Ordering::Acquire))
                || (matches!(&next, Some(Work::Wait { .. }))
                    && WAITING_FOR_STACK.load(Ordering::Acquire))
                || (matches!(&next, Some(Work::RetireOld { .. }))
                    && WAITING_FOR_OLD.load(Ordering::Acquire));
    if next.is_some() && !sleep
    {
        WAKE.store(true, Ordering::Release);
    }
    let active = next.is_some();
    InvocationGate::run(|| {
        ACTIVE.store(active, Ordering::Release);
        *FLIGHT.lock() = next;
    });
    if !active
    {
        super::termination::progress();
    }
}

/// A retained old stack advances only after a real reclamation checkpoint.
pub(crate) fn thread_reclaimed()
{
    if WAITING_FOR_STACK.load(Ordering::Acquire)
    {
        WAKE.store(true, Ordering::Release);
    }
}

/// A per-change hint only. The Work state retains the authentic old binding
/// and rechecks actual directory absence; no copied owner number is authority.
pub(crate) fn owner_removed(owner: &crate::toolbox::handles::OwnerRef)
{
    if WAITING_FOR_OLD.load(Ordering::Acquire)
       && ORIGIN_OWNER.load(Ordering::Acquire) == owner.owner().incarnation()
    {
        WAKE.store(true, Ordering::Release);
    }
}

/// The same boot-held physical pair is ready only after its bounded scrub.
pub(crate) fn backing_ready()
{
    thread_reclaimed();
}

/// The old System-owned report must finish its actual disposal before the
/// recipient can move. Its invalidated ID alone is never an acknowledgment.
pub(crate) fn report_cleanup_wait()
{
    WAITING_FOR_REPORT.store(true, Ordering::Release);
}

pub(crate) fn report_cleanup_progress()
{
    if WAITING_FOR_REPORT.load(Ordering::Acquire)
    {
        WAKE.store(true, Ordering::Release);
    }
}

/// Exact completion notice is only a wake hint; Work retains the unique job.
pub(crate) fn ready_changed(job: u64)
{
    if job != 0 && WAIT_JOB.load(Ordering::Acquire) == job
    {
        WAKE.store(true, Ordering::Release);
    }
}
