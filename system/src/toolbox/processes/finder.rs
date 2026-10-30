//! System-owned, staged shell replacement. Preparation never destroys the
//! report; replacement never reuses a dying owner's image or stack.
#![forbid(unsafe_code)]
use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::*;
use mac_object_table::shared::InvocationGate as _;
use spinning_top::Spinlock;

use super::ResourceBinding;
use crate::{machine::scheduler::{self, PreparedThread},
            toolbox::{handles::InvocationGate, runtime::FinderAdmission}};

/// Unpublished backing is still owned here; published backing remains owned by
/// its canonical THRD row. The admission authenticates both retry stages.
enum PendingThread
{
    Unpublished(PreparedThread),
    Published(u64)
}

enum Work
{
    Requested
    {
        old:      ResourceBinding,
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
    Prepare
    {
        old:       ResourceBinding,
        admission: FinderAdmission,
        loaded:    crate::kernel::LoadedImage
    },
    Commit
    {
        old:       ResourceBinding,
        next:      ResourceBinding,
        admission: FinderAdmission,
        loaded:    crate::kernel::LoadedImage
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
static WAKE: AtomicBool = AtomicBool::new(false);
static WAITING_FOR_STACK: AtomicBool = AtomicBool::new(false);
static WAITING_FOR_REPORT: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);
static RETRY: AtomicBool = AtomicBool::new(false);
static UI_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Called only by the System-owned report action with its directory capability.
pub(crate) fn request(binding: ResourceBinding, permit: crate::allocator::recovery::Permit)
                      -> Result<(), Status>
{
    let activity = crate::context::recovery_activity().ok_or(NOT_SUPPORTED)?;
    InvocationGate::run(|| {
        let mut flight = FLIGHT.lock();
        if ACTIVE.swap(true, Ordering::AcqRel)
        {
            return Err(SERVICE_BUSY);
        }
        WAITING_FOR_REPORT.store(false, Ordering::Release);
        UI_PROGRESS.store(false, Ordering::Release);
        *flight = Some(Work::Requested { old: binding,
                                         permit,
                                         activity });
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
        | Work::Prepare { admission,
                        .. }
        | Work::Commit { admission,
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
                          activity } =>
        {
            match desktop(&old, |desktop| desktop.reserve_finder(old.owner(), permit.clone()))
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
                                           activity })
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
            let target =
                crate::allocator::Target::Owner { account: admission.allocation(),
                                                  class:
                                                      mac_kernel_model::policy::MemoryClass::Kernel };
            match crate::allocator::with_target(target, || {
                      crate::kernel::copy_application(admission.owner(), admission.original())
                  })
            {
                Ok(loaded) =>
                {
                    Some(Work::Prepare { old,
                                         admission,
                                         loaded })
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
        Work::Prepare { old,
                        admission,
                        loaded } =>
        {
            match desktop(&old, |desktop| {
                      desktop.bind_finder_image(&admission, loaded.binding())?;
                      desktop.processes.resource_binding(admission.id())
                  })
            {
                Ok(next) =>
                {
                    Some(Work::Commit { old,
                                        next,
                                        admission,
                                        loaded })
                }
                Err(SERVICE_BUSY) =>
                {
                    Some(Work::Prepare { old,
                                         admission,
                                         loaded })
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
                       admission,
                       loaded } =>
        {
            match desktop(&old, |desktop| desktop.commit_finder_handoff(&admission))
            {
                Ok(()) =>
                {
                    WAITING_FOR_REPORT.store(false, Ordering::Release);
                    Some(Work::Wait { next,
                                      admission,
                                      loaded })
                }
                Err(SERVICE_BUSY) =>
                {
                    Some(Work::Commit { old,
                                        next,
                                        admission,
                                        loaded })
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
        Work::Wait { next,
                     admission,
                     loaded } =>
        {
            WAITING_FOR_STACK.store(true, Ordering::Release);
            if scheduler::has_threads(admission.old())
            {
                return Some(Work::Wait { next,
                                         admission,
                                         loaded });
            }
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
                    Some(Work::Paused { next,
                                        admission,
                                        thread: PendingThread::Unpublished(failure.prepared),
                                        status: failure.status,
                                        attempted: false })
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
                Ok(Ok(())) => None,
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
                          admission,
                          mut last } =>
        {
            if !admission.owner().accepting()
            {
                // Normal owner teardown now owns the registered thread and UI.
                // This committed refresh must not pin its closing account.
                return None;
            }
            UI_PROGRESS.store(false, Ordering::Release);
            match desktop(&next, |desktop| desktop.finish_finder_start_ui(&admission))
            {
                Ok(()) => None,
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
                              admission,
                              loaded,
                              status,
                              mut attempted } =>
        {
            if RETRY.swap(false, Ordering::AcqRel)
            {
                return Some(Work::Wait { next,
                                         admission,
                                         loaded });
            }
            if !attempted || UI_PROGRESS.swap(false, Ordering::AcqRel)
            {
                let _notification = desktop(&next, |desktop| desktop.finder_restart_failed(status));
                attempted = true;
            }
            Some(Work::BackingPaused { next,
                                       admission,
                                       loaded,
                                       status,
                                       attempted })
        }
        Work::Paused { next,
                       admission,
                       thread,
                       status,
                       mut attempted } =>
        {
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
                let _notification = desktop(&next, |desktop| desktop.finder_restart_failed(status));
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
                         Some(Work::Paused { attempted: true,
                                             .. }
                              | Work::BackingPaused { attempted: true,
                                                      .. }
                              | Work::RunningUi { .. }))
                || (matches!(&next, Some(Work::Commit { .. }))
                    && WAITING_FOR_REPORT.load(Ordering::Acquire))
                || (matches!(&next, Some(Work::Wait { .. }))
                    && WAITING_FOR_STACK.load(Ordering::Acquire));
    if next.is_some() && !sleep
    {
        WAKE.store(true, Ordering::Release);
    }
    InvocationGate::run(|| {
        ACTIVE.store(next.is_some(), Ordering::Release);
        *FLIGHT.lock() = next;
    });
}

/// A retained old stack advances only after a real reclamation checkpoint.
pub(crate) fn thread_reclaimed()
{
    if WAITING_FOR_STACK.load(Ordering::Acquire)
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
