//! Combined native event/readiness park. Source snapshots never hold a manager
//! lock together with SCHEDULER. A revision check closes check-before-park races.
#![forbid(unsafe_code)]
use mac_abi::{Status, readiness as wire, *};

use super::*;
#[derive(Default)]
pub(super) struct State
{
    keys:     Vec<(u32, u64)>,
    notified: bool,
    active:   bool
}
static REVISION: AtomicU64 = AtomicU64::new(0);
pub(super) fn advance()
{
    REVISION.fetch_add(1, Ordering::Release);
}
pub(super) fn wake_matching(thread: &Thread, kind: u32, id: u64)
{
    if !thread.state.get().live || !matches!(thread.state.get().waiting, Some(Wait::Events { .. }))
    {
        return;
    }
    let matches = {
        let state = thread.core.readiness.lock();
        state.active
        && state.keys
                .iter()
                .any(|&(k, v)| {
                    v == id && (k == kind || kind == wire::PIPE_READ && k == wire::PIPE_WRITE)
                })
    };
    if matches
    {
        thread.update(|s| {
                  s.waiting = None;
                  s.woken = 1;
              });
        wake::runnable(thread.state.get().requires_primary());
    }
}
pub(crate) fn changed(kind: u32, id: u64)
{
    let scheduler = SCHEDULER.lock();
    advance();
    for (_, thread) in scheduler.iter()
    {
        wake_matching(thread, kind, id);
    }
}

pub(crate) fn request(call: &mut wire::Call, sources: &mut [wire::Source]) -> Result<(), Status>
{
    if call.operation != wire::WAIT && !sources.is_empty()
    {
        return Err(BAD_ARGUMENT);
    }
    if call.operation == wire::CURRENT
    {
        call.thread = SCHEDULER.lock().thread(current()).ok_or(WRONG_THREAD)?.id;
        return Ok(());
    }
    if call.operation == wire::NOTIFY
    {
        let scheduler = SCHEDULER.lock();
        let owner = crate::context::current_thread_context().identity().app.0;
        let (_, thread) = scheduler.iter()
                                   .find(|(_, t)| t.id == call.thread && t.state.get().live)
                                   .ok_or(NOT_FOUND)?;
        if thread.core.binding.owner() != owner
        {
            return Err(ACCESS_DENIED);
        }
        thread.core.readiness.lock().notified = true;
        if matches!(thread.state.get().waiting, Some(Wait::Events { .. }))
        {
            thread.update(|s| {
                      s.waiting = None;
                      s.woken = 1;
                  });
            wake::runnable(thread.state.get().requires_primary());
        }
        return Ok(());
    }
    if call.operation != wire::WAIT || call.event.operation != mac_abi::events::POSITION_WAIT
    {
        return Err(BAD_ARGUMENT);
    }
    let revision = REVISION.load(Ordering::Acquire);
    let owner = crate::context::current_owner()?;
    let mut keys = {
        let scheduler = SCHEDULER.lock();
        let thread = scheduler.thread(current()).ok_or(WRONG_THREAD)?;
        core::mem::take(&mut thread.core.readiness.lock().keys)
    };
    keys.clear();
    keys.try_reserve_exact(sources.len()).map_err(|_| NO_MEMORY)?;
    let mut ready = false;
    let timeout = call.event.timeout_ticks;
    let mut timer_deadline = None;
    let now =
        crate::machine::monitor::ticks_to_nanoseconds(crate::platform::ticks()).map_err(|_| {
                                                                                   BAD_ABI
                                                                               })?;
    for source in sources.iter_mut()
    {
        source.ready = 0;
        match source.kind
        {
            wire::PIPE_READ | wire::PIPE_WRITE =>
            {
                let (id, flags) = crate::toolbox::pipes::readiness(&owner, *source)?;
                source.ready = flags;
                keys.push((source.kind, id));
            }
            wire::CHILD =>
            {
                if source.minimum != 0
                {
                    return Err(BAD_ARGUMENT);
                }
                source.ready = crate::toolbox::processes::child_readiness(source.id)?;
                keys.push((wire::CHILD, source.id));
            }
            wire::TIMER =>
            {
                if source.minimum != 0
                {
                    return Err(BAD_ARGUMENT);
                }
                let (flags, deadline) = crate::toolbox::timers::readiness(&owner, source.id)?;
                source.ready = flags;
                keys.push((wire::TIMER, source.id));
                if let Some(deadline) = deadline
                {
                    let tick =
                        crate::machine::monitor::nanoseconds_to_ticks(deadline).map_err(|_| {
                                                                                   BAD_ARGUMENT
                                                                               })?;
                    timer_deadline = Some(timer_deadline.map_or(tick, |old: u64| old.min(tick)));
                }
            }
            wire::DEADLINE =>
            {
                if source.minimum != 0
                {
                    return Err(BAD_ARGUMENT);
                }
                if now >= source.id
                {
                    source.ready = wire::READY;
                }
                else
                {
                    let deadline =
                        crate::machine::monitor::nanoseconds_to_ticks(source.id).map_err(|_| {
                                                                                    BAD_ARGUMENT
                                                                                })?;
                    timer_deadline =
                        Some(timer_deadline.map_or(deadline, |old: u64| old.min(deadline)));
                }
            }
            _ => return Err(BAD_ARGUMENT)
        }
        ready |= source.ready != 0;
    }
    crate::kernel::with_toolbox(|t| {
  let status=t.positioned_event(&mut call.event);
  if status!=OK {return Err(status);}
  let scheduler=SCHEDULER.lock();
  let thread=scheduler.thread(current()).ok_or(WRONG_THREAD)?;
  service::suspension_allowed(thread.state.get())?;
  let mut state=thread.core.readiness.lock();
  call.notified=u32::from(core::mem::take(&mut state.notified));
  // Replaced storage drops outside all resident guards.
  core::mem::swap(&mut state.keys,&mut keys);
  call.event.waited=0;state.active=false;
  if ready || call.notified!=0 || call.event.present!=0 || timeout==0
    || REVISION.load(Ordering::Acquire)!=revision {return Ok(());}
  let duration=super::super::monitor::event_ticks_to_ticks(t.control_layout_wait(timeout)).map_err(|_|BAD_ARGUMENT)?;
  let event_deadline=crate::platform::ticks().checked_add(duration).ok_or(BAD_ARGUMENT)?;
  let deadline=timer_deadline.map_or(event_deadline,|d|d.min(event_deadline));
  thread.update(|s|{s.waiting=Some(Wait::Events {mask:call.event.mask,deadline});s.woken=0;});
  call.event.waited=1;state.active=true;
  Ok(())
 }).map_err(|e|e.status())?
}

/// Detach retained wait-source storage after admission closes, releasing its
/// allocation outside SCHEDULER before the owner's resource walk.
pub(super) fn cancel_owner(owner: u64)
{
    let count = SCHEDULER.lock().threads.len();
    for index in 0..count
    {
        let keys = {
            let scheduler = SCHEDULER.lock();
            let Some(thread) = scheduler.thread(index).filter(|t| t.core.binding.owner() == owner)
            else
            {
                continue;
            };
            core::mem::take(&mut thread.core.readiness.lock().keys)
        };
        drop(keys);
    }
}

pub(super) fn disarm(thread: &Thread)
{
    thread.core.readiness.lock().active = false;
}
