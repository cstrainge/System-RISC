//! Deferred publication after the reporter's own modal cleanup. Retain the
//! genuine binding and frozen full diagnostic; only real modal release wakes
//! another attempt. No application handler or per-tick retry participates.
#![forbid(unsafe_code)]
use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::{SERVICE_BUSY, Status, WindowId, crashes::CrashInfo};
use mac_object_table::shared::InvocationGate as _;
use spinning_top::Spinlock;

use super::Snapshot;
use crate::toolbox::{handles::InvocationGate, processes::ResourceBinding};

struct Pending
{
    binding:  ResourceBinding,
    snapshot: Snapshot,
    flags:    u32,
    last:     Option<Status>
}
static PENDING: Spinlock<Option<Pending>> = Spinlock::new(None);
static WAKE: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Transfer the actual queued report to the terminating owner's controller.
/// A second publisher must not reset a Manager after its live window exists.
pub(crate) fn take_for_termination(binding: &ResourceBinding) -> Result<Option<Snapshot>, Status>
{
    let pending = InvocationGate::run(|| {
        if RUNNING.load(Ordering::Acquire)
        {
            return Err(SERVICE_BUSY);
        }
        let mut slot = PENDING.lock();
        if slot.as_ref().is_some_and(|pending| {
                            pending.binding.ledger().owner() == binding.ledger().owner()
                            && pending.binding.image().map(crate::images::Binding::id)
                               == binding.image().map(crate::images::Binding::id)
                        })
        {
            Ok(slot.take())
        }
        else
        {
            Ok(None)
        }
    })?;
    Ok(pending.map(|mut pending| {
                  pending.snapshot.add_flags(pending.flags);
                  pending.snapshot
              }))
}

pub(super) fn queue(binding: ResourceBinding, snapshot: Snapshot, flags: u32)
                    -> Result<CrashInfo, Status>
{
    let info = snapshot.info();
    let pending = Pending { binding,
                            snapshot,
                            flags,
                            last: None };
    let accepted = InvocationGate::run(|| {
        let mut slot = PENDING.lock();
        // Publication has temporarily moved the unique slot outside this lock.
        // Refuse another admission until it either completes or restores it.
        if RUNNING.load(Ordering::Acquire)
        {
            return Err(SERVICE_BUSY);
        }
        if let Some(previous) = slot.as_mut()
        {
            if previous.binding.ledger().owner() != pending.binding.ledger().owner()
            {
                return Err(SERVICE_BUSY);
            }
            previous.flags |= flags;
            return Ok(false);
        }
        *slot = Some(pending);
        Ok(true)
    })?;
    if accepted
    {
        WAKE.store(true, Ordering::Release);
        serial!("CRASH_REPORT_PENDING app={} thread={} bytes={} reason=own-modal-cleanup",
                info.app, info.thread, info.report_bytes);
    }
    Ok(CrashInfo { window: WindowId(0),
                   dismissed: 0,
                   ..info })
}
pub(crate) fn modal_available()
{
    crate::toolbox::processes::termination::progress();
    WAKE.store(true, Ordering::Release);
}
pub(crate) fn owner_stopped()
{
    WAKE.store(true, Ordering::Release);
    DISMISS_WAKE.store(true, Ordering::Release);
}

fn cancel_unpublished(pending: Pending)
{
    let info = pending.snapshot.info();
    serial!("CRASH_REPORT_PUBLICATION_CANCELLED app={} thread={} bytes={} reason=owner-stopped; \
             no replacement owner or Activity was fabricated",
            info.app,
            info.thread,
            info.report_bytes);
    // Native snapshots retain the protected stock. Its normal Drop archives
    // the immutable bytes, outside the PENDING gate; no live Window ticket is
    // owned by this unpublished queue entry.
    drop(pending);
}
struct Running;
impl Drop for Running
{
    fn drop(&mut self)
    {
        RUNNING.store(false, Ordering::Release);
        crate::toolbox::processes::termination::progress();
    }
}
pub(crate) fn pump_reports()
{
    if !WAKE.load(Ordering::Acquire) || RUNNING.swap(true, Ordering::AcqRel)
    {
        return;
    }
    let _running = Running;
    WAKE.store(false, Ordering::Release);
    let pending = InvocationGate::run(|| PENDING.lock().take());
    let Some(mut pending) = pending
    else
    {
        return;
    };
    if !pending.binding.ledger().accepting()
    {
        cancel_unpublished(pending);
        return;
    }
    let result = (|| {
        let mut snapshot = pending.snapshot.retain()?;
        snapshot.add_flags(pending.flags);
        let activity = crate::recovery::claim_report(pending.binding.ledger().clone())?;
        let shown = activity.run(|| {
                                crate::kernel::with_desktop(|desktop| {
                                    desktop.publish_pending_crash_report(&pending.binding, snapshot)
                                }).map_err(|e| e.status())?
                            })
                            .and_then(core::convert::identity)
                            .and_then(|window| {
                                if pending.binding.ledger().accepting()
                                {
                                    Ok(window)
                                }
                                else
                                {
                                    Err(mac_abi::OPERATION_CANCELLED)
                                }
                            });
        match shown
        {
            Ok(window) =>
            {
                activity.set_object(window.0);
                Ok(())
            }
            Err(status) =>
            {
                // Rendering may have published a window before a later draw
                // refused. Close it through the normal manager; its genuine
                // ticket/Activity remains retained if disposal refuses.
                let _cleanup = activity.run(|| {
                                           crate::kernel::with_desktop(|desktop| {
                                               desktop.close_crash_report(&activity)
                                           }).map_err(|error| error.status())?
                                       });
                activity.close();
                Err(status)
            }
        }
    })();
    if let Err(status) = result
    {
        // Owner stop can win while this unique entry is outside PENDING and
        // RUNNING is set. Do not restore an impossible publication forever.
        if !pending.binding.ledger().accepting()
        {
            cancel_unpublished(pending);
            return;
        }
        if pending.last != Some(status)
        {
            serial!("CRASH_REPORT_PUBLICATION_RETAINED app={} status={status}; full report and \
                     binding retained",
                    pending.binding.owner());
            pending.last = Some(status);
        }
        InvocationGate::run(|| *PENDING.lock() = Some(pending));
    }
}

struct Dismissal
{
    binding:  ResourceBinding,
    activity: crate::recovery::Activity,
    snapshot: Snapshot,
    window:   WindowId,
    last:     Option<Status>
}
static DISMISSAL: Spinlock<Option<Dismissal>> = Spinlock::new(None);
static DISMISS_WAKE: AtomicBool = AtomicBool::new(false);
static DISMISS_RUNNING: AtomicBool = AtomicBool::new(false);

pub(super) fn queue_dismissal(binding: ResourceBinding, activity: crate::recovery::Activity,
                              snapshot: Snapshot, window: WindowId)
                              -> Result<(), Status>
{
    let admitted = InvocationGate::run(|| {
        let mut slot = DISMISSAL.lock();
        if DISMISS_RUNNING.load(Ordering::Acquire)
        {
            return Err(SERVICE_BUSY);
        }
        if let Some(held) = slot.as_ref()
        {
            if held.binding.ledger().owner() != binding.ledger().owner()
               || !held.activity.same_session(&activity)
               || held.window != window
            {
                return Err(SERVICE_BUSY);
            }
            return Ok(false);
        }
        *slot = Some(Dismissal { binding,
                                 activity,
                                 snapshot,
                                 window,
                                 last: None });
        Ok(true)
    })?;
    if admitted
    {
        DISMISS_WAKE.store(true, Ordering::Release);
    }
    Ok(())
}

/// Canonical completion wakes a retained close even while its own Activity
/// prevents the modal session from becoming available. No timer retry is used.
pub(crate) fn cleanup_progress()
{
    crate::toolbox::processes::termination::progress();
    crate::toolbox::processes::finder::cleanup_progress();
    DISMISS_WAKE.store(true, Ordering::Release);
}
struct Dismissing;
impl Drop for Dismissing
{
    fn drop(&mut self)
    {
        DISMISS_RUNNING.store(false, Ordering::Release);
        crate::toolbox::processes::termination::progress();
    }
}
pub(crate) fn pump_dismissals()
{
    if !DISMISS_WAKE.load(Ordering::Acquire) || DISMISS_RUNNING.swap(true, Ordering::AcqRel)
    {
        return;
    }
    let _running = Dismissing;
    DISMISS_WAKE.store(false, Ordering::Release);
    let pending = InvocationGate::run(|| DISMISSAL.lock().take());
    let Some(mut pending) = pending
    else
    {
        return;
    };
    // All capabilities remain local until scope entry succeeds. In particular,
    // a reclaimed selection cannot lose the original binding or cleanup intent.
    let result = pending.activity
                        .run(|| {
                            crate::kernel::with_desktop(|desktop| {
                                desktop.finish_crash_dismissal(&pending.binding, &pending.activity,
                                                               pending.window, &pending.snapshot)
                            }).map_err(|error| error.status())?
                        })
                        .and_then(core::convert::identity);
    if let Err(status) = result
    {
        if pending.last != Some(status)
        {
            serial!("CRASH_REPORT_DISMISS_RETAINED app={} status={status}; original session, \
                     diagnostic and Window ACK barrier retained",
                    pending.binding.owner());
            pending.last = Some(status);
        }
        InvocationGate::run(|| *DISMISSAL.lock() = Some(pending));
    }
}
