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
    pipe:      u64,
    turn:      Turn
}

/// One invocation's deadline survives every replay and spurious peer wake.
#[derive(Clone, Copy)]
pub(crate) struct Turn
{
    pub(crate) resumed: bool,
    wait_nanoseconds:   Option<u64>,
    deadline:           Option<u64>
}
impl Turn
{
    pub(crate) fn expired(self) -> bool
    {
        self.deadline.is_some_and(|deadline| crate::platform::ticks() >= deadline)
    }
}

/// Consume only an exact replay. Calculate a fresh deadline outside the
/// scheduler lock, never on replay (which would extend the caller's timeout).
pub(crate) fn begin(identity: RequestIdentity, handle: u64, operation: u32,
                    wait_nanoseconds: Option<u64>)
                    -> Result<Turn, Status>
{
    let record = {
        let scheduler = SCHEDULER.lock();
        if let Some(thread) = scheduler.thread(current())
        {
            let record = thread.core.pending_pipe.lock().take();
            if record.is_some() && matches!(thread.state.get().waiting, Some(Wait::Pipe { .. }))
            {
                thread.update(|state| state.waiting = None);
            }
            record
        }
        else
        {
            None
        }
    };
    if let Some(record) = record
    {
        if record.identity != identity
           || record.handle != handle
           || record.operation != operation
           || record.turn.wait_nanoseconds != wait_nanoseconds
        {
            return Err(mac_abi::BAD_ARGUMENT);
        }
        return Ok(Turn { resumed: true,
                         ..record.turn });
    }
    let deadline =
        match wait_nanoseconds
        {
            Some(value) if value != u64::MAX =>
            {
                Some(service::relative_deadline(value / 1_000_000_000,
                                                (value % 1_000_000_000) as u32).map_err(|cause| {
                                                                                   cause.status()
                                                                               })?)
            }
            _ => None
        };
    Ok(Turn { resumed: false,
              wait_nanoseconds,
              deadline })
}

/// Park the calling thread until the pipe's state changes. Called with the pipe
/// manager guard held, so no peer can change the condition before the wait is
/// published.
pub(crate) fn park(pipe: u64, handle: u64, operation: u32, identity: RequestIdentity,
                   suspension: Result<(), Status>, turn: Turn)
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
                                  pipe,
                                  turn });
    }
    thread.update(|state| {
              state.waiting = Some(Wait::Pipe { pipe,
                                                deadline: turn.deadline });
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
        if !matches!(thread.state.get().waiting, Some(Wait::Pipe { pipe: waiting, .. }) if waiting == pipe)
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
