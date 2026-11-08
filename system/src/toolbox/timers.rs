//! Canonical Time Manager state. Tasks are owner-ledger objects; removing a
//! task invalidates every outstanding observation and wakes its subscribers.
#![forbid(unsafe_code)]
use mac_abi::{Status, readiness as ready, timers as wire, *};
use mac_object_table::{domain::ReleasedResource,
                       registry::{Release, Source, Specification}};
use spinning_top::Spinlock;

use crate::toolbox::handles::{CapacityPolicy, KindCode, OwnerRef, Store};
#[derive(Clone, Copy)]
struct Task
{
    id:       u64,
    extended: bool,
    deadline: Option<u64>,
    anchor:   Option<u64>,
    saved:    Option<u64>
}
static MANAGER: spin::Once<Spinlock<Store<Task>>> = spin::Once::new();
fn status(e: mac_object_table::domain::PoolError) -> Status
{
    crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(e))
}
fn manager() -> Result<&'static Spinlock<Store<Task>>, Status>
{
    MANAGER.try_call_once(|| {
               let (k, b) = crate::toolbox::handles::timer_kinds()?;
               Ok(Spinlock::new(Store::owned(k, b).map_err(status)?))
           })
}
fn now() -> Result<u64, Status>
{
    crate::machine::monitor::ticks_to_nanoseconds(crate::platform::ticks()).map_err(|_| BAD_ABI)
}
fn owned(store: &Store<Task>, id: u64, owner: &OwnerRef) -> Result<Task, Status>
{
    let domain = store.owner(id).ok_or(NOT_FOUND)?;
    if domain.incarnation() != owner.owner().incarnation()
    {
        return Err(ACCESS_DENIED);
    }
    store.get(id).copied().ok_or(NOT_FOUND)
}
pub(crate) fn request(call: &mut wire::Call) -> Result<(), Status>
{
    if call.extended > 1
    {
        return Err(BAD_ARGUMENT);
    }
    if call.state != 0
       || call.remaining != 0
       || call.operation != wire::INSERT
          && (call.extended != 0 || call.deadline != 0 || call.anchor != 0)
       || !matches!(call.operation, wire::PRIME | wire::RESET) && call.delay != 0
    {
        return Err(BAD_ARGUMENT);
    }
    let owner = crate::context::current_owner()?;
    if !owner.accepting()
    {
        return Err(OPERATION_CANCELLED);
    }
    let now = now()?;
    let mut store = manager()?.lock();
    if call.operation == wire::INSERT
    {
        if call.id != 0
           || call.delay != 0
           || call.extended == 0 && (call.deadline != 0 || call.anchor != 0)
        {
            return Err(BAD_ARGUMENT);
        }
        call.id = store.insert_with(&owner, |id| {
                           Task { id,
                                  extended: call.extended != 0,
                                  deadline: None,
                                  anchor: (call.anchor != 0).then_some(call.anchor),
                                  saved: (call.deadline != 0).then_some(call.deadline) }
                       })
                       .map_err(status)?;
        call.state = 0;
        return Ok(());
    }
    let task = owned(&store, call.id, &owner)?;
    let (result, removed) = match call.operation
    {
        wire::PRIME | wire::RESET =>
        {
            if call.operation == wire::PRIME && task.deadline.is_some()
            {
                return Err(BAD_ARGUMENT);
            }
            let delta = if call.delay < 0
            {
                u64::from(call.delay.unsigned_abs()) * 1000
            }
            else
            {
                call.delay as u64 * 1_000_000
            };
            let deadline = if call.operation == wire::PRIME && task.extended && call.delay == 0
            {
                task.saved.unwrap_or(now)
            }
            else
            {
                let base = if call.operation == wire::PRIME && task.extended
                {
                    task.anchor.unwrap_or(now)
                }
                else
                {
                    now
                };
                base.checked_add(delta).ok_or(BAD_ARGUMENT)?
            };
            let value = store.get_mut(call.id).ok_or(NOT_FOUND)?;
            value.deadline = Some(deadline);
            value.saved = None;
            if call.operation == wire::RESET
            {
                value.anchor = None;
            }
            call.deadline = deadline;
            call.state = 1;
            (Ok(()), None)
        }
        wire::FIRE =>
        {
            call.state = 0;
            if let Some(deadline) = task.deadline.filter(|d| now >= *d)
            {
                let value = store.get_mut(call.id).ok_or(NOT_FOUND)?;
                value.deadline = None;
                value.anchor = Some(deadline);
                call.state = 1;
                call.anchor = deadline;
            }
            (Ok(()), None)
        }
        wire::REMOVE =>
        {
            call.state = u32::from(task.deadline.is_some());
            call.deadline = task.deadline.unwrap_or(0);
            call.remaining = task.deadline.map_or(0, |d| d.saturating_sub(now));
            call.anchor = task.anchor.unwrap_or(0);
            (Ok(()), Some(store.remove(call.id).ok_or(NOT_FOUND)?))
        }
        _ => (Err(BAD_ARGUMENT), None)
    };
    drop(store);
    drop(removed);
    if result.is_ok() && (call.operation != wire::FIRE || call.state != 0)
    {
        crate::machine::scheduler::readiness::changed(ready::TIMER, call.id);
    }
    result
}
/// The deadline is canonical, not a stale userspace copy. Removed timers are
/// invalid; installed inactive tasks remain asleep until primed or removed.
pub(crate) fn readiness(owner: &OwnerRef, id: u64) -> Result<(u32, Option<u64>), Status>
{
    let now = now()?;
    let store = manager()?.lock();
    let task = match owned(&store, id, owner)
    {
        Ok(t) => t,
        Err(NOT_FOUND) => return Ok((ready::INVALID, None)),
        Err(e) => return Err(e)
    };
    let flags = if task.deadline.is_some_and(|d| now >= d) { ready::READY } else { 0 };
    Ok((flags, task.deadline))
}
#[derive(Clone, Copy)]
pub(crate) enum Registration
{
    Task,
    Storage
}
impl Registration
{
    pub(crate) const ALL: [Self; 2] = [Self::Task, Self::Storage];

    pub(crate) fn specification(self) -> Specification<'static>
    {
        let (kind, label, capacity) = match self
        {
            Self::Task => (*b"TMTK", "Time Manager tasks", CapacityPolicy::GrowableUntilMemory),
            Self::Storage =>
            {
                (*b"TMST",
                 "Time Manager backing",
                 CapacityPolicy::HardBound { slots:  1,
                                             reason: "One Time Manager backing per owner domain" })
            }
        };
        Specification { kind: KindCode(kind),
                        label,
                        provider: "System Time Manager",
                        capacity,
                        source: Source::Resident }
    }
}
impl Release<crate::toolbox::runtime::Toolbox, crate::toolbox::handles::cleanup::Failure>
    for Registration
{
    fn release(&self, _: &mut crate::toolbox::runtime::Toolbox, ticket: ReleasedResource)
               -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        use crate::toolbox::handles::cleanup::Failure;
        let store = match manager()
        {
            Ok(s) => s,
            Err(_) => return Err(Failure::context_unavailable(ticket, "Time Manager"))
        };
        let mut store = store.lock();
        if matches!(self, Self::Storage)
        {
            return store.release_storage(ticket).map_err(Into::into);
        }
        let removed = store.release(ticket).map_err(Failure::from)?;
        let id = removed.id;
        drop(store);
        drop(removed);
        crate::machine::scheduler::readiness::changed(ready::TIMER, id);
        Ok(())
    }
}
