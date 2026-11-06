//! A pending join owns its actual right read; raw IDs and wake hints never
//! reconstruct the operation. Every detach/drop leaves outside scheduler guards.
use super::*;
impl Drop for PendingJoin
{
    fn drop(&mut self)
    {
        let mut claim = self.right.claimant.lock();
        if *claim == Claim::Joining(self.waiter)
        {
            *claim = Claim::Open;
        }
    }
}
fn take(executor: &crate::context::ApplicationExecutor<'_>) -> Result<Option<PendingJoin>, Error>
{
    let scheduler = SCHEDULER.lock();
    let thread = current_row(&scheduler, executor)?;
    let pending = thread.core.pending_join.lock().take();
    if pending.is_some()
    {
        thread.update(|state| {
                  if matches!(state.waiting, Some(Wait::NativeJoin { .. }))
                  {
                      state.waiting = None;
                  }
              });
    }
    Ok(pending)
}
pub(crate) fn query(executor: &crate::context::ApplicationExecutor<'_>, call: &mut wire::Call)
                    -> Result<(), Error>
{
    let right = manager::read(executor.owner(), call.right)?;
    let phase = right.completion.phase.load(Ordering::Acquire);
    call.thread = right.completion.thread.load(Ordering::Acquire);
    call.state = phase;
    call.outcome =
        if phase == wire::ABORTED { wire::ABORTED_OUTCOME } else { wire::NORMAL_OUTCOME };
    Ok(())
}
pub(crate) fn join(executor: &crate::context::ApplicationExecutor<'_>, id: u64,
                   identity: service::RequestIdentity)
                   -> Result<Option<u64>, Error>
{
    if !executor.owner().accepting()
    {
        return Err(Error::Stopped);
    }
    let pending = match take(executor)?
    {
        Some(pending) =>
        {
            if pending.identity != identity
               || pending.id != id
               || pending.waiter != executor.thread()
            {
                return Err(Error::Invalid("join replay changed the original invocation or right"));
            }
            pending
        }
        None =>
        {
            PendingJoin { identity,
                          id,
                          waiter: executor.thread(),
                          right: manager::read(executor.owner(), id)? }
        }
    };
    let target = pending.right.completion.thread.load(Ordering::Acquire);
    let completed = {
        let scheduler = SCHEDULER.lock();
        let thread = current_row(&scheduler, executor)?;
        service::suspension_allowed(thread.state.get()).map_err(|status| {
            Error::Admission { status,
                               stage: "join suspension" }
        })?;
        if target == executor.thread()
        {
            return Err(Error::SelfJoin);
        }
        let phase = pending.right.completion.phase.load(Ordering::Acquire);
        if phase == wire::PREPARING
        {
            return Err(Error::Busy);
        }
        if phase == wire::ABORTED
        {
            return Err(Error::Aborted);
        }
        let mut claim = pending.right.claimant.lock();
        match *claim
        {
            Claim::Open => *claim = Claim::Joining(executor.thread()),
            Claim::Joining(waiter) if waiter == executor.thread() => (),
            Claim::Consumed => return Err(Error::Detached),
            _ => return Err(Error::Busy)
        }
        if phase == wire::COMPLETE
        {
            *claim = Claim::Consumed;
            true
        }
        else
        {
            drop(claim);
            // Completion and park commitment share this exact scheduler turn.
            // Its release producer cannot pass us before the actual owner is set.
            *thread.core.pending_join.lock() = Some(pending);
            thread.update(|state| {
                      state.waiting = Some(Wait::NativeJoin { request: target });
                      state.woken = 0;
                  });
            return Ok(None);
        }
    };
    if completed
    {
        manager::consume_read(&pending.right, id)?;
    }
    drop(pending);
    Ok(Some(target))
}
pub(crate) fn detach(executor: &crate::context::ApplicationExecutor<'_>, id: u64)
                     -> Result<(), Error>
{
    let right = manager::read(executor.owner(), id)?;
    {
        let _scheduler = SCHEDULER.lock();
        let mut claim = right.claimant.lock();
        let waiter = match *claim
        {
            Claim::Joining(waiter) => Some(waiter),
            _ => None
        };
        *claim = Claim::Consumed;
        if let Some(waiter) = waiter
        {
            if let Some((_, thread)) = _scheduler.iter().find(|(_, thread)| thread.id == waiter)
            {
                let owns = thread.core.pending_join.lock().as_ref().is_some_and(|pending| {
                                                                       pending.id == id
                                                                && Arc::ptr_eq(&pending.right.right,
                                                                               &right.right)
                                                                   });
                if owns && matches!(thread.state.get().waiting, Some(Wait::NativeJoin { .. }))
                {
                    thread.update(|state| {
                              state.waiting = None;
                              state.woken = 1;
                          });
                }
            }
        }
    }
    manager::consume_read(&right, id)?;
    drop(right);
    Ok(())
}
/// Request decode/admission failure cancels only the original operation owned
/// by this canonical continuation. Its read/claim leave after SCHEDULER ends.
pub(crate) fn cancel_current() -> bool
{
    let pending = {
        let scheduler = SCHEDULER.lock();
        let Some(thread) = scheduler.thread(current())
        else
        {
            return false;
        };
        let pending = thread.core.pending_join.lock().take();
        if pending.is_some()
        {
            thread.update(|state| {
                      if matches!(state.waiting, Some(Wait::NativeJoin { .. }))
                      {
                          state.waiting = None;
                      }
                  });
        }
        pending
    };
    let cancelled = pending.is_some();
    drop(pending);
    cancelled
}
pub(crate) fn cancel_owner(owner: u64)
{
    let count = SCHEDULER.lock().threads.len();
    for slot in 0..count
    {
        let pending = {
            let scheduler = SCHEDULER.lock();
            let Some(thread) = scheduler.thread(slot).filter(|thread| {
                                                         thread.core.binding.owner() == owner
                                                         && !thread.core.context.is_module_worker()
                                                     })
            else
            {
                continue;
            };
            let pending = thread.core.pending_join.lock().take();
            if pending.is_some()
            {
                thread.update(|state| {
                          if matches!(state.waiting, Some(Wait::NativeJoin { .. }))
                          {
                              state.waiting = None;
                          }
                      });
            }
            pending
        };
        drop(pending);
    }
}
