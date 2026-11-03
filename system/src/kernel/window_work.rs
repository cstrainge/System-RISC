//! Guard-free completion of canonical Window work. Only actual admitted debt
//! is visited after a real edge; no input, timer tick or all-window scan drives
//! cleanup. Actual Removed payloads and affine UI receipts leave Desktop before
//! destruction/ACK, including when an original native caller has gone away.
#![forbid(unsafe_code)]
use mac_abi::{NOT_FOUND, SERVICE_BUSY};

use crate::toolbox::windows::manager::operations::{Progress, WindowJob};

fn desktop<R>(call: impl FnOnce(&mut crate::toolbox::runtime::Toolbox) -> R)
              -> Result<R, crate::state::AccessError>
{
    super::with_window_toolbox_owned((), |(), toolbox| call(toolbox)).map_err(|(cause, ())| cause)
}

pub(super) fn pump()
{
    if super::desktop::take_desktop_returned()
    {
        crate::machine::scheduler::callbacks::desktop_returned();
    }
    if !super::TOOLBOX.work_ready()
    {
        return;
    }
    let pending = desktop(|toolbox| {
        if super::desktop::take_source_returned()
        {
            toolbox.window_resources_returned();
            toolbox.windows.source_cleanup_changed();
        }
        toolbox.windows.take_work_pending()
    });
    if !matches!(pending, Ok(true))
    {
        return;
    }

    // The input owner consumes the same common observer, not another notice or
    // a synthesized event. Its affine checkout retains the original gesture.
    crate::toolbox::runtime::pump_window_input();
    crate::toolbox::runtime::pump_ordinary_paint();

    let Ok(count) = desktop(|toolbox| toolbox.windows.removal_work_len())
    else
    {
        return;
    };
    // The bound is this admitted work-list length. Completing work does not
    // remove rows until the final detached-payload phase below.
    for index in 0..count
    {
        let Ok(job) = desktop(|toolbox| toolbox.windows.removal_work(index))
        else
        {
            return;
        };
        let Some(job) = job
        else
        {
            if let Ok(Some(handle)) = desktop(|toolbox| toolbox.windows.trusted_close_work(index))
            {
                advance_trusted(&handle);
                continue;
            }
            advance_birth(index);
            continue;
        };
        advance(&job);
        finish_quiesced(&job);
        finish_ui(&job);
        job.deliver_wake();
    }
    drain_removed();
    drain_retired_births();
    drain_retired_acknowledgements();
}

fn advance_trusted(handle: &crate::toolbox::windows::manager::trusted::TrustedCloseHandle)
{
    if handle.ready()
    {
        // Arm before the actual Desktop attempt. Its genuine return wakes this
        // same header if the borrow refuses before the child preflight runs.
        handle.arm_wait();
        match desktop(|toolbox| toolbox.advance_trusted_window_close(handle))
        {
            Ok(Ok(())) | Ok(Err(NOT_FOUND | SERVICE_BUSY)) | Err(_) => (),
            Ok(Err(status)) =>
            {
                serial!("TRUSTED_WINDOW_CLOSE_RETAINED window={} status={status}",
                        handle.window_id().0)
            }
        }
    }
    if let Some(receipt) = handle.take_close_cleanup()
    {
        receipt.arm_wait();
        match desktop(|toolbox| toolbox.cleanup_user_window_close(&receipt))
        {
            Ok(Ok(removed)) =>
            {
                drop(removed);
                receipt.finish();
            }
            _ => receipt.defer()
        }
    }
    handle.deliver_wake();
}

fn drain_retired_acknowledgements()
{
    loop
    {
        let Ok(retired) = desktop(|toolbox| toolbox.windows.take_retired_acknowledgement())
        else
        {
            return;
        };
        let Some(retired) = retired
        else
        {
            return;
        };
        // Original owner, source proof and callback/header references retire
        // only after authoritative completion, outside the Desktop guard.
        drop(retired);
    }
}

/// Abandoned unpublished birth: the SAME owner observes/disposes its actual
/// admitted author work outside Desktop. A Wait keeps the owner in canonical
/// debt; no payload is dropped or acknowledged without its real proof.
fn advance_birth(index: usize)
{
    use crate::toolbox::windows::manager::birth::CleanupProgress;

    let Ok(owner) = desktop(|toolbox| toolbox.windows.birth_work(index))
    else
    {
        return;
    };
    let Some(owner) = owner
    else
    {
        return;
    };
    match owner.advance_cleanup()
    {
        Ok(CleanupProgress::Wait | CleanupProgress::Finished) => (),
        Err(status) =>
        {
            serial!("WINDOW_BIRTH_CLEANUP_RETAINED window={} status={status}", owner.window_id().0);
        }
    }
}

fn drain_retired_births()
{
    loop
    {
        let Ok(retired) = desktop(|toolbox| toolbox.windows.take_retired_birth())
        else
        {
            return;
        };
        let Some(retired) = retired
        else
        {
            return;
        };
        // Unpublished payload/permit and prepared callbacks destruct here,
        // after every Desktop guard has ended.
        drop(retired);
    }
}

fn advance(job: &alloc::sync::Arc<WindowJob>)
{
    if !job.ready()
    {
        return;
    }
    let admitted = desktop(|toolbox| toolbox.prepare_user_window_removal(job));
    match admitted
    {
        Ok(Ok(())) => (),
        // Completed rows have no releasing ticket; their original UI debt is
        // handled independently below. Busy retains the actual canonical job.
        Ok(Err(NOT_FOUND | SERVICE_BUSY)) | Err(_) => return,
        Ok(Err(status)) =>
        {
            serial!("WINDOW_REMOVAL_ADMISSION_RETAINED window={} status={status}",
                    job.window_id().0);
            return;
        }
    }
    match job.advance()
    {
        Ok(Progress::Complete(mut completion)) =>
        {
            let committed = desktop(|toolbox| {
                toolbox.prepare_user_window_removal(job)?;
                toolbox.windows.finish_user_close(&mut completion).map_err(|cause| {
                    serial!("WINDOW_REMOVAL_COMMIT_RETAINED window={} cause={cause}", job.window_id().0);
                    // Original typed cause remains in WM's original ticket/job;
                    // this status controls only this service turn's refusal.
                    SERVICE_BUSY
                })
            });
            // A refused commit puts the SAME Reply back, outside Desktop.
            drop(completion);
            let _ = committed;
        }
        Ok(Progress::Pending | Progress::Finished(_)) | Err(SERVICE_BUSY) => (),
        Err(status) =>
        {
            serial!("WINDOW_AUTHOR_OPERATION_RETAINED window={} status={status}",
                    job.window_id().0);
        }
    }
}

fn finish_quiesced(job: &alloc::sync::Arc<WindowJob>)
{
    let Ok(Ok(mut work)) = desktop(|toolbox| toolbox.windows.quiescence_work(job))
    else
    {
        return;
    };
    // A refusal keeps the SAME source plan in the canonical job. Never drop
    // and recreate a plan to turn its temporary reader into a progress event.
    if work.acquire().is_err()
    {
        work.defer();
        return;
    }
    let id = work.window_id();
    let finished = desktop(|toolbox| {
        toolbox.prepare_user_window_removal(job)?;
        toolbox.windows.finish_quiesced_close(id, work.proof())
            .map_err(|cause| {
                serial!("WINDOW_QUIESCED_COMMIT_RETAINED window={} cause={cause}", id.0);
                SERVICE_BUSY
            })
    });
    if !matches!(finished, Ok(Ok(_)))
    {
        work.defer();
    }
    // Actual Loading/definition destruction, or restoring the same proof to
    // canonical debt, occurs outside Desktop on every result.
    drop(work);
}

fn finish_ui(job: &alloc::sync::Arc<WindowJob>)
{
    let Some(receipt) = job.take_close_cleanup()
    else
    {
        return;
    };
    receipt.arm_wait();
    match desktop(|toolbox| toolbox.cleanup_user_window_close(&receipt))
    {
        Ok(Ok(removed_dialog)) =>
        {
            // Real dialog destruction/ACK precedes recording the UI tail done.
            drop(removed_dialog);
            receipt.finish();
        }
        Ok(Err(status)) =>
        {
            receipt.defer();
            if status != SERVICE_BUSY
            {
                serial!("WINDOW_CLOSE_UI_RETAINED window={} status={status}", job.window_id().0);
            }
        }
        Err(_) => receipt.defer()
    }
}

fn drain_removed()
{
    loop
    {
        let Ok(removed) = desktop(|toolbox| toolbox.windows.take_removed())
        else
        {
            return;
        };
        let Some(removed) = removed
        else
        {
            return;
        };
        // Drops the actual retained payload, definition binding and original
        // resource ACK before the manager observes its existing completion token.
        drop(removed);
        if desktop(|toolbox| toolbox.windows.removed_dropped()).is_err()
        {
            // Removal Drop has already published the genuine return notice.
            return;
        }
    }
}
