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
    Requested(ResourceBinding),
    Image
    {
        old:       ResourceBinding,
        admission: FinderAdmission
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
        prepared:  PreparedThread
    },
    Wait
    {
        next:      ResourceBinding,
        admission: FinderAdmission,
        prepared:  PreparedThread
    },
    Started
    {
        next:      ResourceBinding,
        admission: FinderAdmission,
        thread:    u64
    },
    Failure
    {
        target: ResourceBinding,
        status: Status
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
static RUNNING: AtomicBool = AtomicBool::new(false);
static RETRY: AtomicBool = AtomicBool::new(false);

/// Called only by the System-owned report action with its directory capability.
pub(crate) fn request(binding: ResourceBinding) -> Result<(), Status>
{
    InvocationGate::run(|| {
        let mut flight = FLIGHT.lock();
        if let Some(Work::Paused { next,
                                   .. }) = flight.as_ref()
           && next.owner() == binding.owner()
        {
            RETRY.store(true, Ordering::Release);
            WAKE.store(true, Ordering::Release);
            return Ok(());
        }
        if ACTIVE.swap(true, Ordering::AcqRel)
        {
            return Err(SERVICE_BUSY);
        }
        *flight = Some(Work::Requested(binding));
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
    match work
    {
        Work::Requested(old) =>
        {
            match desktop(&old, |desktop| desktop.reserve_finder(old.owner()))
            {
                Ok(admission) =>
                {
                    Some(Work::Image { old,
                                       admission })
                }
                Err(SERVICE_BUSY) => Some(Work::Requested(old)),
                Err(status) =>
                {
                    Some(Work::Failure { target: old,
                                         status })
                }
            }
        }
        Work::Image { old,
                      admission } =>
        {
            match crate::kernel::copy_application(admission.owner(), admission.original())
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
                                         status })
                }
            }
        }
        Work::Prepare { old,
                        admission,
                        loaded } =>
        {
            let next = match desktop(&old, |desktop| {
                      desktop.bind_finder_image(&admission, loaded.binding())?;
                      desktop.processes.resource_binding(admission.id())
                  })
            {
                Ok(next) => next,
                Err(SERVICE_BUSY) =>
                {
                    return Some(Work::Prepare { old,
                                                admission,
                                                loaded });
                }
                Err(status) =>
                {
                    return Some(Work::Failure { target: old,
                                                status });
                }
            };
            match scheduler::prepare(admission.owner().clone(), loaded)
            {
                Ok(prepared) =>
                {
                    Some(Work::Commit { old,
                                        next,
                                        admission,
                                        prepared })
                }
                Err(failure) =>
                {
                    let scheduler::PrepareFailure { status,
                                                    loaded } = failure;
                    drop(loaded);
                    Some(Work::Failure { target: old,
                                         status })
                }
            }
        }
        Work::Commit { old,
                       next,
                       admission,
                       prepared } =>
        {
            match desktop(&old, |desktop| desktop.commit_finder_handoff(&admission))
            {
                Ok(()) =>
                {
                    Some(Work::Wait { next,
                                      admission,
                                      prepared })
                }
                Err(SERVICE_BUSY) =>
                {
                    Some(Work::Commit { old,
                                        next,
                                        admission,
                                        prepared })
                }
                Err(status) =>
                {
                    Some(Work::Failure { target: old,
                                         status })
                }
            }
        }
        Work::Wait { next,
                     admission,
                     prepared } =>
        {
            WAITING_FOR_STACK.store(true, Ordering::Release);
            if scheduler::has_threads(admission.old())
            {
                return Some(Work::Wait { next,
                                         admission,
                                         prepared });
            }
            WAITING_FOR_STACK.store(false, Ordering::Release);
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
                    serial!("FINDER_RESTART_RETAINED app={} status={}; prepared image and stack \
                             retained for retry",
                            admission.id(),
                            failure.status);
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
                Err(status) =>
                {
                    // The actual thread exists. Keep its receipt and finalize
                    // metadata later; never issue a second replacement thread.
                    if status != SERVICE_BUSY
                    {
                        serial!("FINDER_START_PUBLICATION_RETAINED app={} thread={thread} \
                                 status={status}",
                                admission.id());
                    }
                    Some(Work::Started { next,
                                         admission,
                                         thread })
                }
            }
        }
        Work::Failure { target,
                        status } =>
        {
            match desktop(&target, |desktop| desktop.finder_restart_failed(status))
            {
                Ok(()) => None,
                Err(_) =>
                {
                    Some(Work::Failure { target,
                                         status })
                }
            }
        }
        Work::Paused { next,
                       admission,
                       prepared,
                       status,
                       mut shown } =>
        {
            if RETRY.swap(false, Ordering::AcqRel)
            {
                return Some(Work::Wait { next,
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
                                             .. }))
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
