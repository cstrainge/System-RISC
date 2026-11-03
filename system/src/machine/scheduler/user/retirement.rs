//! User backing retires only after actual CPU/service quiescence. Original
//! mapping owners and the unique THRD ticket survive every refusal.
#![forbid(unsafe_code)]
use alloc::sync::{Arc, Weak};
use core::sync::atomic::Ordering;

use mac_abi::{BAD_ABI, SERVICE_BUSY, Status};

use super::super::*;
use crate::machine::{monitor, paging::user::Error};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::machine::scheduler) enum Stage
{
    StackPublish,
    DescriptorPublish,
    StackRetire,
    DescriptorRetire
}

/// The original typed cause plus SAME-operation lifecycle quarantine evidence.
/// A Busy monitor result after PTE writes is never mistaken for admission Busy.
#[derive(Debug)]
pub(in crate::machine::scheduler) struct Failure
{
    cause:      Error,
    quarantine: Option<crate::machine::paging::modules::CleanupCause>
}
impl Failure
{
    pub(in crate::machine::scheduler) fn mapping(cause: Error,
                                                 mapping: &crate::machine::paging::user::Mapping)
                                                 -> Self
    {
        Self { cause,
               quarantine: mapping.quarantine_cause() }
    }

    pub(in crate::machine::scheduler) fn rejected(status: Status) -> Self
    {
        Self { cause:      Error::Rejected(status),
               quarantine: None }
    }

    pub(in crate::machine::scheduler) fn status(&self) -> Status
    {
        self.cause.status()
    }
}
impl core::fmt::Display for Failure
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        self.cause.fmt(out)?;
        if let Some(cause) = &self.quarantine
        {
            write!(out, "; original mapping quarantined: {cause}")?;
        }
        Ok(())
    }
}
fn retryable(failure: &Failure) -> bool
{
    failure.quarantine.is_none()
    && matches!(&failure.cause,
                Error::Readers(_)
                | Error::Rejected(SERVICE_BUSY)
                | Error::Monitor { cause: monitor::Error::Busy,
                                   .. })
}
fn permanent(core: &ThreadCore) -> bool
{
    matches!(core.cleanup_refusal.lock().as_ref(),
             Some(ReapState::UserMapping { terminal: true,
                                           .. }))
}
/// Only ABI projection returns Status; the actual owned cause stays on Core.
pub(in crate::machine::scheduler) fn refuse(core: &ThreadCore, stage: Stage, cause: Failure)
                                            -> Status
{
    let status = cause.status();
    let terminal = !retryable(&cause);
    // Never overwrite an original uncertain PTE/ACK failure with a later Busy.
    let mut terminal_notice = None;
    let (previous, status) = {
        let mut slot = core.cleanup_refusal.lock();
        if let Some(ReapState::UserMapping { terminal: true,
                                             cause: original,
                                             .. }) = slot.as_ref()
        {
            // The refusal still describes the original uncertain operation.
            // A later admission Busy cannot project a different failure to the
            // caller while that same terminal diagnosis remains retained.
            (None, original.status())
        }
        else
        {
            let previous = slot.replace(ReapState::UserMapping { stage,
                                                                 cause,
                                                                 terminal,
                                                                 retired_count_released: false });
            if let Some(ReapState::UserMapping { stage,
                                                 cause,
                                                 terminal: true,
                                                 .. }) = slot.as_ref()
            {
                terminal_notice = Some((*stage, cause.status()));
            }
            (previous, status)
        }
    };
    drop(previous);
    if let Some((stage, status)) = terminal_notice
    {
        // First permanent transition only. No I/O or payload destruction runs
        // under the Core's failure-record lock; ordinary retry edges stay quiet.
        serial!("THREAD_USER_MAPPING_FAILED thread={} stage={stage:?} status={status}; original \
                 mapping and cleanup ticket retained",
                core.context.identity().thread.0);
    }
    status
}
fn clear_retryable(core: &ThreadCore)
{
    let previous = {
        let mut slot = core.cleanup_refusal.lock();
        if matches!(slot.as_ref(),
                    Some(ReapState::UserMapping { terminal: false,
                                                  .. }))
        {
            slot.take()
        }
        else
        {
            None
        }
    };
    drop(previous);
}

pub(in crate::machine::scheduler) fn publish(core: &ThreadCore) -> Result<(), Status>
{
    let stack = {
                    let backing = core.backing.lock();
                    backing.as_ref().map(|backing| backing.stack.clone())
                }.ok_or_else(|| refuse(core, Stage::StackPublish, Failure::rejected(BAD_ABI)))?;
    match &core.runtime
    {
        Some(runtime) =>
        {
            runtime.publish().map_err(|cause| {
                                  serial!("THREAD_USER_PUBLISH_RETAINED stage=descriptor \
                                           cause={cause}");
                                  refuse(core, Stage::DescriptorPublish, cause)
                              })?
        }
        None if core.context.is_module_worker() => (),
        None => return Err(refuse(core, Stage::DescriptorPublish, Failure::rejected(BAD_ABI)))
    }
    stack.publish_user().map_err(|cause| {
                            serial!("THREAD_USER_PUBLISH_RETAINED stage=stack cause={cause}");
                            refuse(core, Stage::StackPublish, cause)
                        })
}

fn retire_maps(core: &ThreadCore) -> Result<(), (Stage, Failure)>
{
    let stack = {
                    let backing = core.backing.lock();
                    backing.as_ref().map(|b| b.stack.clone())
                }.ok_or((Stage::StackRetire, Failure::rejected(BAD_ABI)))?;
    let result = stack.retire_user().map_err(|cause| (Stage::StackRetire, cause));
    drop(stack);
    result?;
    if let Some(runtime) = &core.runtime
    {
        super::Runtime::retire_if_last(runtime).map_err(|cause| (Stage::DescriptorRetire, cause))?;
    }
    Ok(())
}

// A reaper's own selected Arc is not a reader-release event. Arm only after
// observing another actual Core reader, whose last-drop wake our Arc may mask.
struct Candidate
{
    core:     Arc<ThreadCore>,
    released: CandidateRelease
}
struct CandidateRelease
{
    core:   Weak<ThreadCore>,
    notify: bool
}
impl Candidate
{
    fn new(core: Arc<ThreadCore>) -> Self
    {
        Self { released: CandidateRelease { core:   Arc::downgrade(&core),
                                            notify: false },
               core }
    }
}
impl Drop for CandidateRelease
{
    fn drop(&mut self)
    {
        if self.notify && self.core.strong_count() == 1
        {
            context_released();
        }
    }
}
fn identity(thread: &Thread, core: &Arc<ThreadCore>, id: u64) -> bool
{
    let state = thread.state.get();
    thread.id == id
    && Arc::ptr_eq(&thread.core, core)
    && state.retired
    && state.claimed_by.is_none()
    && state.service.quiescent()
}

fn quarantine(scheduler: &mut Scheduler, slot: usize, id: u64, core: &Arc<ThreadCore>)
{
    let Some(thread) = scheduler.thread(slot).filter(|thread| identity(thread, core, id))
    else
    {
        return;
    };
    let ticket = thread.core.release.lock().take();
    if let Some(ticket) = ticket
    {
        match scheduler.records.as_ref()
        {
            Some(store) =>
            {
                if let Err(error) = store.quarantine(ticket)
                {
                    serial!("THREAD_USER_QUARANTINE_RETAINED thread={id} cause={error}; original \
                             ticket retained");
                    *core.release.lock() = Some(error.into_ticket());
                }
            }
            None =>
            {
                *core.release.lock() = Some(ticket);
            }
        }
    }
    // This records removal from the retryable reaper count, NOT a translation
    // or payload ACK. The canonical row/map/ticket remain quarantined/retained.
    let mut state = core.cleanup_refusal.lock();
    if let Some(ReapState::UserMapping { terminal: true,
                                         retired_count_released,
                                         .. }) = state.as_mut()
    {
        if !*retired_count_released
        {
            if RETIRED_COUNT.try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                            .is_ok()
            {
                *retired_count_released = true;
            }
        }
    }
}

pub(in crate::machine::scheduler) fn reclaim()
{
    if critical_active() || !RECLAIM_PENDING.swap(false, Ordering::AcqRel)
    {
        return;
    }
    address::release_retired();
    let mut after = 0;
    loop
    {
        let selected = {
            let scheduler = SCHEDULER.lock();
            let selected = scheduler.iter().find_map(|(slot,thread)|{
                if slot<after {return None;}
                after=slot+1;
                let state=thread.state.get();
                if !state.retired || state.claimed_by.is_some() || !state.service.quiescent()
                   || thread.core.user_fault.lock().is_some() {return None;}
                if matches!(thread.core.cleanup_refusal.lock().as_ref(),Some(ReapState::UserMapping {terminal:true,retired_count_released:true,..}))
                   && thread.core.release.lock().is_none() {return None;}
                Some((slot,thread.id,Candidate::new(thread.core.clone())))
            });
            selected
        };
        let Some((slot, id, mut candidate)) = selected
        else
        {
            break;
        };
        let core = &candidate.core;
        // No CPU/service can still use this context. Its own transport pins
        // must leave before asking their exact stack/descriptor maps to drain.
        drop(core.context.abandon_user_call());
        if let Some(queue) = &core.callback_queue
        {
            crate::toolbox::resources::definitions::callbacks::stopped(queue);
        }
        if !permanent(core)
        {
            if let Err((stage, cause)) = retire_maps(core)
            {
                if !retryable(&cause)
                {
                    serial!("THREAD_USER_RETIRE_RETAINED thread={id} stage={stage:?} \
                             cause={cause}");
                }
                refuse(core, stage, cause);
            }
            else
            {
                clear_retryable(core);
            }
        }
        let removed = {
            let mut scheduler = SCHEDULER.lock();
            let Some(thread) = scheduler.thread(slot).filter(|thread| identity(thread, core, id))
            else
            {
                continue;
            };
            if permanent(core)
            {
                quarantine(&mut scheduler, slot, id, core);
                continue;
            }
            // Two Core references are expected: canonical THRD and this exact
            // candidate. No extra Context/Stack reference belongs to the reaper.
            let readers = Arc::strong_count(core);
            if readers != 2 || Arc::strong_count(&core.context) != 1
            {
                candidate.released.notify = readers > 2;
                continue;
            }
            let stack_ready =
                core.backing.lock().as_ref().is_some_and(|b| Arc::strong_count(&b.stack) == 1);
            if !stack_ready
            {
                continue;
            }
            // A recoverable mapping refusal remains owned and cannot release
            // the canonical ticket merely because unrelated readers drained.
            if core.cleanup_refusal
                   .lock()
                   .as_ref()
                   .is_some_and(|cause| matches!(cause, ReapState::UserMapping { .. }))
            {
                continue;
            }
            let Some(ticket) = thread.core.release.lock().take()
            else
            {
                continue;
            };
            match scheduler.records.as_mut()
            {
                Some(store) =>
                {
                    match store.release(ticket)
                    {
                        Ok(removed) =>
                        {
                            scheduler.threads[slot] = None;
                            RETIRED_COUNT.fetch_sub(1, Ordering::AcqRel);
                            Some(removed)
                        }
                        Err(error) =>
                        {
                            serial!("THREAD_RECLAIM_REFUSED: {error}; original ticket retained");
                            match store.quarantine(error.into_ticket())
                            {
                                Ok(()) => (),
                                Err(error) =>
                                {
                                    *core.release.lock() = Some(error.into_ticket());
                                }
                            }
                            None
                        }
                    }
                }
                None =>
                {
                    *core.release.lock() = Some(ticket);
                    None
                }
            }
        };
        if let Some(removed) = removed
        {
            let context = core.context.clone();
            let app = context.identity().app.0;
            drop(candidate);
            context.release();
            drop(context);
            drop(removed);
            serial!("PROCESS_THREAD_RECLAIMED app={app} thread={id}");
            crate::toolbox::processes::retirement::thread_reclaimed();
        }
    }
}
