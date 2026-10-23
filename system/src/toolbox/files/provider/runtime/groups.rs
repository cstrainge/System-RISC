//! Owned provider-group continuations. No caller array or manager borrow
//! survives submission. Errors drain the driver's actual undo/commit outcome.
use fs::{IconMovePhase as Phase, IconMoveProgress as GroupProgress};

use super::*;
pub(super) struct State
{
    handle:      u32,
    volume:      u32,
    operation:   u32,
    draining:    bool,
    error:       Status,
    may_publish: bool
}
fn retire(handle: u32) -> Result<(), Status>
{
    files(|m| m.groups.remove(handle).map(|_| ()).ok_or(NOT_FOUND))
}
fn quarantine(handle: u32)
{
    let _ = files(|m| {
        m.quarantine_group(handle);
        Ok(())
    });
}
fn failed(mut flight: Flight, mut state: State, error: Status) -> Progress
{
    if error == ROLLBACK_FAILED
    {
        quarantine(state.handle);
        return Progress::Done(Err(error));
    }
    state.draining = true;
    if state.error == OK
    {
        state.error = error;
    }
    if files(|m| {
           let group = m.groups.get_mut(state.handle).ok_or(NOT_FOUND)?;
           group.running = true;
           group.abandoned |= flight.abandoned;
           Ok(())
       }).is_err()
    {
        quarantine(state.handle);
        return Progress::Done(Err(ROLLBACK_FAILED));
    }
    match submit_args(flight.provider,
                      Operation::AbortIconMoves,
                      Arguments { volume: state.volume,
                                  token: state.handle,
                                  ..Default::default() })
    {
        Ok(job) =>
        {
            flight.job = job;
            flight.step = Step::Group(state);
            Progress::Pending(flight)
        }
        Err(_) =>
        {
            quarantine(state.handle);
            Progress::Done(Err(ROLLBACK_FAILED))
        }
    }
}
pub(super) fn advance(mut flight: Flight, result: Result<Vec<u8>, Status>, never_sent: bool)
                      -> Progress
{
    let Step::Group(state) = core::mem::replace(&mut flight.step, Step::Count)
    else
    {
        return Progress::Done(Err(BAD_ABI));
    };
    if never_sent && state.operation == BEGIN_ICON_GROUP && !state.draining
    {
        return Progress::Done(retire(state.handle).and(Err(OPERATION_CANCELLED)));
    }
    let result = result.and_then(|bytes| GroupProgress::decode(&bytes).map_err(|_| BAD_ABI));
    let progress = match result
    {
        Ok(progress) => progress,
        Err(NOT_FOUND) if state.draining && !state.may_publish =>
        {
            return Progress::Done(retire(state.handle).and(Err(if state.error == OK
                                                               {
                                                                   OPERATION_CANCELLED
                                                               }
                                                               else
                                                               {
                                                                   state.error
                                                               })));
        }
        Err(error) if state.draining =>
        {
            quarantine(state.handle);
            return Progress::Done(Err(if error == ROLLBACK_FAILED
                                      {
                                          error
                                      }
                                      else
                                      {
                                          ROLLBACK_FAILED
                                      }));
        }
        Err(error) => return failed(flight, state, error)
    };
    if let Err(error) = files(|m| {
        m.ready_group(state.handle, progress)?;
        if progress.terminal()
        {
            m.groups.remove(state.handle).ok_or(NOT_FOUND)?;
        }
        else if state.draining || flight.abandoned
        {
            m.groups.get_mut(state.handle).ok_or(NOT_FOUND)?.running = true;
        }
        Ok(())
    })
    {
        return failed(flight, state, error);
    }
    if progress.terminal()
    {
        if progress.phase == Phase::Aborted && state.error != OK
        {
            return Progress::Done(Err(state.error));
        }
        return Progress::Done(Ok(Reply::Group { handle: state.handle,
                                                progress }));
    }
    if state.draining || flight.abandoned
    {
        return failed(flight, state, OPERATION_CANCELLED);
    }
    Progress::Done(Ok(Reply::Group { handle: state.handle,
                                     progress }))
}
fn deliver(call: &mut IconGroupCall, progress: Progress) -> Status
{
    match progress
    {
        Progress::Pending(flight) => waiting(flight),
        Progress::Done(Ok(Reply::Group { handle,
                                         progress })) =>
        {
            call.handle = handle;
            super::super::super::groups::fill(call, progress);
            OK
        }
        Progress::Done(Err(error)) => error,
        _ => BAD_ABI
    }
}
fn key(call: &IconGroupCall) -> Key
{
    Key { record:    Record::Group,
          operation: call.operation,
          volume:    call.volume,
          index:     call.count,
          cnid:      0,
          handle:    call.handle,
          fork:      0,
          capacity:  0,
          cursor:    DirectoryScanCursor::default(),
          resource:  None,
          when:      call.when,
          metadata:  None }
}
pub(crate) fn request(owner: u64, call: &mut IconGroupCall, rows: &[IconMove]) -> Status
{
    let thread = crate::context::current_thread().0;
    if thread <= 1
    {
        return WRONG_THREAD;
    }
    if call.operation == ABANDON_ICON_GROUP
    {
        return files(|m| m.group_request(owner, call, rows)).err().unwrap_or(OK);
    }
    let key = key(call);
    match claim(owner, thread, key, "")
    {
        Ok(Some(flight)) => return deliver(call, super::advance(flight)),
        Ok(None) => (),
        Err(error) => return error
    }
    match begin(owner, thread, key, call, rows)
    {
        Ok(flight) => waiting(flight),
        Err(error) => error
    }
}

pub(super) fn begin(owner: u64, thread: u64, key: Key, call: &IconGroupCall, rows: &[IconMove])
                    -> Result<Flight, Status>
{
    (|| {
        // All request allocations precede admission, let alone publication.
        let mut payload = Vec::new();
        if call.operation == BEGIN_ICON_GROUP
        {
            payload.try_reserve_exact(4 + rows.len() * fs::ICON_MOVE_BYTES)
                   .map_err(|_| NO_MEMORY)?;
            payload.resize(4 + rows.len() * fs::ICON_MOVE_BYTES, 0);
            payload[..4].copy_from_slice(&(rows.len() as u32).to_le_bytes());
            for (entry, bytes) in
                rows.iter().zip(payload[4..].chunks_exact_mut(fs::ICON_MOVE_BYTES))
            {
                fs::IconMove { id:              entry.cnid,
                               expected_parent: entry.expected_parent,
                               parent:          entry.parent,
                               placement:       entry.placement,
                               horizontal:      entry.point.h,
                               vertical:        entry.point.v }.encode(bytes)
                                                               .map_err(|_| BAD_ARGUMENT)?;
            }
        }
        let (provider, handle, volume, when) = files(|m| {
            let handle = if call.operation == BEGIN_ICON_GROUP
            {
                let mut initial = *call;
                m.reserve_group(owner, &mut initial, rows)?;
                initial.handle
            }
            else
            {
                call.handle
            };
            let group = m.groups.get_mut(handle).ok_or(NOT_FOUND)?;
            if group.owner != owner
            {
                return Err(ACCESS_DENIED);
            }
            if group.quarantined
            {
                return Err(ROLLBACK_FAILED);
            }
            if group.running
            {
                return Err(SERVICE_BUSY);
            }
            group.running = true;
            Ok((group.provider.ok_or(BAD_ABI)?, handle, group.volume, group.when))
        })?;
        let operation = match call.operation
        {
            BEGIN_ICON_GROUP => Operation::BeginIconMoves,
            STEP_ICON_GROUP => Operation::StepIconMoves,
            CANCEL_ICON_GROUP => Operation::AbortIconMoves,
            _ => return Err(BAD_ARGUMENT)
        };
        let submitted = submit_args(provider,
                                    operation,
                                    Arguments { volume,
                                                token: handle,
                                                when: if call.operation == BEGIN_ICON_GROUP
                                                {
                                                    when
                                                }
                                                else
                                                {
                                                    0
                                                },
                                                payload: &payload,
                                                ..Default::default() });
        let job = match submitted
        {
            Ok(job) => job,
            Err(error) =>
            {
                if call.operation == BEGIN_ICON_GROUP
                {
                    let _ = retire(handle);
                }
                else
                {
                    let _ = files(|m| {
                        m.groups.get_mut(handle).ok_or(NOT_FOUND)?.running = false;
                        Ok(())
                    });
                }
                return Err(error);
            }
        };
        Ok(Flight { thread,
                    owner,
                    key,
                    provider,
                    job,
                    input: String::new(),
                    path: None,
                    abandoned: false,
                    step: Step::Group(State { handle,
                                              volume,
                                              operation: call.operation,
                                              draining: false,
                                              error: OK,
                                              may_publish: call.operation
                                                           != BEGIN_ICON_GROUP }) })
    })()
}
pub(super) fn pump_cleanup()
{
    {
        let mut flights = FLIGHTS.lock();
        if flights.len() >= MAX_FLIGHTS
        {
            return;
        }
        if flights.capacity() < MAX_FLIGHTS
        {
            let additional = MAX_FLIGHTS - flights.len();
            if flights.try_reserve_exact(additional).is_err()
            {
                return;
            }
        }
    }
    let prepared = files(|m| {
        let mark = m.groups.iter().find(|(_, group)| {
                                      group.abandoned
                                      && !group.running
                                      && !group.quarantined
                                      && group.provider.is_some()
                                  });
        let Some((handle, group)) = mark
        else
        {
            return Ok(None);
        };
        let values = (handle, group.volume, group.provider.unwrap(), group.owner);
        m.groups.get_mut(handle).ok_or(NOT_FOUND)?.running = true;
        Ok(Some(values))
    });
    let (handle, volume, provider, owner) = match prepared
    {
        Ok(Some(values)) => values,
        Ok(None) =>
        {
            super::super::super::groups::idle_cleanup();
            return;
        }
        Err(_) => return
    };
    match submit_args(provider,
                      Operation::AbortIconMoves,
                      Arguments { volume,
                                  token: handle,
                                  ..Default::default() })
    {
        Ok(job) =>
        {
            let call = IconGroupCall { operation: CANCEL_ICON_GROUP,
                                       handle,
                                       volume,
                                       ..Default::default() };
            FLIGHTS.lock().push(Flight { thread: 0,
                                         owner,
                                         key: key(&call),
                                         provider,
                                         job,
                                         input: String::new(),
                                         path: None,
                                         abandoned: true,
                                         step: Step::Group(State { handle,
                                                                   volume,
                                                                   operation:
                                                                       CANCEL_ICON_GROUP,
                                                                   draining: true,
                                                                   error: OK,
                                                                   may_publish: true }) });
            REAP.store(true, Ordering::Release);
        }
        Err(_) => quarantine(handle)
    }
}
