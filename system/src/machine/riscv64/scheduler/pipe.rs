//! Pipe waits. A blocked reader or writer parks through the ordinary scheduler
//! wait mechanism; the pipe manager wakes its peers after releasing its own
//! guard, so no pipe state is inspected under the scheduler lock.
#![forbid(unsafe_code)]
use mac_abi::Status;

use super::{service::RequestIdentity, *};

/// The caller's original parked invocation. Storage is Copy; no frame, buffer
/// or manager borrow is retained across the wait.
#[derive(Clone, Copy)]
pub(super) struct Pending
{
    identity:  RequestIdentity,
    handle:    u64,
    operation: u32,
    pipe:      u64
}

/// Consume this thread's pending pipe wait when the same request is replayed.
/// A changed handle or operation is refused rather than applied to a different
/// endpoint than the one that parked.
pub(crate) fn begin(identity: RequestIdentity, handle: u64, operation: u32)
                    -> Result<bool, Status>
{
    let scheduler = SCHEDULER.lock();
    let Some(thread) = scheduler.thread(current())
    else
    {
        return Ok(false);
    };
    let record = {
        let Some(record) = thread.core.pending_pipe.lock().take()
        else
        {
            return Ok(false);
        };
        record
    };
    if matches!(thread.state.get().waiting, Some(Wait::Pipe { .. }))
    {
        thread.update(|state| state.waiting = None);
    }
    if record.identity != identity || record.handle != handle || record.operation != operation
    {
        return Err(mac_abi::BAD_ARGUMENT);
    }
    Ok(true)
}

/// Park the calling thread until the pipe's state changes. Called with the pipe
/// manager guard held, so no peer can change the condition before the wait is
/// published.
pub(crate) fn park(pipe: u64, handle: u64, operation: u32, identity: RequestIdentity,
                   suspension: Result<(), Status>)
                   -> Result<(), Status>
{
    suspension?;
    let scheduler = SCHEDULER.lock();
    let Some(thread) = scheduler.thread(current())
    else
    {
        return Err(mac_abi::WRONG_THREAD);
    };
    service::suspension_allowed(thread.state.get())?;
    {
        let mut pending = thread.core.pending_pipe.lock();
        if pending.is_some()
        {
            return Err(mac_abi::SERVICE_BUSY);
        }
        *pending = Some(Pending { identity,
                                  handle,
                                  operation,
                                  pipe });
    }
    thread.update(|state| {
              state.waiting = Some(Wait::Pipe { pipe });
              state.woken = 0;
          });
    Ok(())
}

/// Make every thread parked on `pipe` runnable. Allocation-free and safe from a
/// guard-free outer return; the woken request re-checks the real condition.
pub(crate) fn wake(pipe: u64)
{
    let scheduler = SCHEDULER.lock();
    for (_, thread) in scheduler.iter()
    {
        if !thread.state.get().live
        {
            continue;
        }
        if !matches!(thread.state.get().waiting, Some(Wait::Pipe { pipe: waiting }) if waiting == pipe)
        {
            continue;
        }
        if thread.core.pending_pipe.lock().as_ref().is_none_or(|pending| pending.pipe != pipe)
        {
            continue;
        }
        thread.update(|state| {
                  state.waiting = None;
                  state.woken = 1;
              });
        wake::runnable(thread.state.get().requires_primary());
    }
}

/// A malformed replay cannot leave the next valid request permanently Busy.
pub(crate) fn cancel_current()
{
    let scheduler = SCHEDULER.lock();
    let Some(thread) = scheduler.thread(current())
    else
    {
        return;
    };
    if thread.core.pending_pipe.lock().take().is_none()
    {
        return;
    }
    if matches!(thread.state.get().waiting, Some(Wait::Pipe { .. }))
    {
        thread.update(|state| state.waiting = None);
    }
}

/// Stop closes admission first; each abandoned wait is detached in turn.
pub(super) fn cancel_owner(owner: u64)
{
    let scheduler = SCHEDULER.lock();
    for (_, thread) in scheduler.iter()
    {
        if thread.core.context.identity().app.0 != owner
        {
            continue;
        }
        if thread.core.pending_pipe.lock().take().is_none()
        {
            continue;
        }
        if matches!(thread.state.get().waiting, Some(Wait::Pipe { .. }))
        {
            thread.update(|state| state.waiting = None);
        }
    }
}

pub(super) fn release_retired()
{
    let scheduler = SCHEDULER.lock();
    for (_, thread) in scheduler.iter()
    {
        if !thread.state.get().retired
        {
            continue;
        }
        if thread.core.pending_pipe.lock().take().is_none()
        {
            continue;
        }
        if matches!(thread.state.get().waiting, Some(Wait::Pipe { .. }))
        {
            thread.update(|state| state.waiting = None);
        }
    }
}
