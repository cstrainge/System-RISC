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

enum Work
{
    Requested
    {
        old:      ResourceBinding,
        permit:   crate::allocator::recovery::Permit,
        activity: crate::recovery::Activity
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
        shown:     bool
    },
    Paused
    {
        next:      ResourceBinding,
        admission: FinderAdmission,
        prepared:  PreparedThread,
        status:    Status,
        shown:     bool
    }
}
static FLIGHT: Spinlock<Option<Work>> = Spinlock::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static WAKE: AtomicBool = AtomicBool::new(false);
static WAITING_FOR_STACK: AtomicBool = AtomicBool::new(false);
static WAITING_FOR_REPORT: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);
static RETRY: AtomicBool = AtomicBool::new(false);

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
        Work::Image { admission,
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
                    Some(Work::Image { old,
                                       admission })
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
                                                      shown: false });
                }
            };
            WAITING_FOR_STACK.store(false, Ordering::Release);
            let charges =
                match desktop(&next, |desktop| desktop.processes.thread_charges(admission.id()))
                {
                    Ok(charges) => charges,
                    Err(status) =>
                    {
                        return Some(Work::BackingPaused { next,
                                                          admission,
                                                          loaded,
                                                          status,
                                                          shown: false });
                    }
                };
            match scheduler::prepare_recovery(admission.owner().clone(), loaded, charges, loan)
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
                                               shown: false })
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
                                        prepared: failure.prepared,
                                        status: failure.status,
                                        shown: false })
                }
            }
        }
        Work::Started { next,
                        mut admission,
                        thread } =>
        {
            match desktop(&next, |desktop| {
                      desktop.finder_started(&admission, thread)
                             .and_then(|()| scheduler::start_prepared(thread, admission.owner()))
                  })
            {
                Ok(()) =>
                {
                    admission.started();
                    None
                }
                Err(_) =>
                {
                    Some(Work::Started { next,
                                         admission,
                                         thread })
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
                              mut shown } =>
        {
            if RETRY.swap(false, Ordering::AcqRel)
            {
                return Some(Work::Wait { next,
                                         admission,
                                         loaded });
            }
            if !shown
            {
                shown = desktop(&next, |desktop| desktop.finder_restart_failed(status)).is_ok();
            }
            Some(Work::BackingPaused { next,
                                       admission,
                                       loaded,
                                       status,
                                       shown })
        }
        Work::Paused { next,
                       admission,
                       prepared,
                       status,
                       mut shown } =>
        {
            if RETRY.swap(false, Ordering::AcqRel)
            {
                return Some(Work::Publish { next,
                                            admission,
                                            prepared });
            }
            if !shown
            {
                shown = desktop(&next, |desktop| desktop.finder_restart_failed(status)).is_ok();
            }
            Some(Work::Paused { next,
                                admission,
                                prepared,
                                status,
                                shown })
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
                         Some(Work::Paused { shown: true,
                                             .. }
                              | Work::BackingPaused { shown: true,
                                                      .. }))
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
