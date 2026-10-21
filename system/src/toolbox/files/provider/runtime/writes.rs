//! Owned write state machine. App buffers end at the trap; provider callbacks,
//! waits, cancellation and exit cleanup run outside all File Manager guards.
#![forbid(unsafe_code)]
use super::{super::writes::Plan, *};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage
{
    Parent,
    Lookup,
    Begin,
    Chunk,
    Finish,
    Abort
}
pub(super) struct State
{
    plan:        Option<Plan>,
    handle:      u32,
    volume:      u32,
    stage:       Stage,
    written:     usize,
    remote:      bool,
    may_publish: bool,
    error:       Status
}
pub(in crate::toolbox::files::provider) fn request_write_cleanup()
{
    WRITE_CLEANUP.store(true, Ordering::Release);
}
fn retire(handle: u32) -> Result<(), Status>
{
    files(|manager| manager.retire_provider_write(handle))
}
fn quarantine(handle: u32)
{
    let _ = files(|manager| manager.quarantine_provider_write(handle));
}
fn terminal(state: State, result: Result<Reply, Status>) -> Progress
{
    Progress::Done(retire(state.handle).and(result))
}
fn failed(mut flight: Flight, mut state: State, error: Status) -> Progress
{
    if !state.remote
    {
        return terminal(state, Err(error));
    }
    state.stage = Stage::Abort;
    state.error = error;
    let job = submit_args(flight.provider,
                          Operation::AbortWrite,
                          Arguments { volume: state.volume,
                                      token: state.handle,
                                      ..Default::default() });
    match job
    {
        Ok(job) =>
        {
            flight.job = job;
            flight.step = Step::Write(state);
            Progress::Pending(flight)
        }
        Err(_) =>
        {
            quarantine(state.handle);
            Progress::Done(Err(ROLLBACK_FAILED))
        }
    }
}
fn queued(mut flight: Flight, mut state: State, op: Operation, job: Result<OwnedJob, Status>)
          -> Progress
{
    match job
    {
        Ok(job) =>
        {
            if matches!(op, Operation::BeginCreate | Operation::BeginReplace)
            {
                state.remote = true;
            }
            if op == Operation::FinishWrite
            {
                state.may_publish = true;
            }
            flight.job = job;
            flight.step = Step::Write(state);
            Progress::Pending(flight)
        }
        Err(error) => failed(flight, state, error)
    }
}
fn begin(flight: Flight, mut state: State) -> Progress
{
    let plan = state.plan.as_ref().unwrap();
    let args = Arguments { volume: plan.volume,
                           parent: plan.parent,
                           token: plan.handle,
                           length: plan.data.len() as u64,
                           when: plan.when,
                           ..Default::default() };
    let (op, job) = if plan.expected != 0
    {
        let op = Operation::BeginReplace;
        (op,
         submit_args(flight.provider,
                     op,
                     Arguments { parent: 0,
                                 node: plan.expected,
                                 flags: fs::PRESERVE_RESOURCE_FORK,
                                 ..args }))
    }
    else
    {
        let result = (|| {
            let name = core::str::from_utf8(&plan.name).map_err(|_| BAD_ABI)?;
            let mut props = fs::FileProperties::fresh(name, plan.when);
            props.finder_info[..4].copy_from_slice(&plan.file_type);
            props.finder_info[4..8].copy_from_slice(&plan.creator);
            let mut bytes = Vec::new();
            let count = fs::PROPERTIES_BYTES + name.len();
            bytes.try_reserve_exact(count).map_err(|_| NO_MEMORY)?;
            bytes.resize(count, 0);
            props.encode(&mut bytes).map_err(|_| BAD_ARGUMENT)?;
            submit_args(flight.provider,
                        Operation::BeginCreate,
                        Arguments { payload: &bytes,
                                    ..args })
        })();
        (Operation::BeginCreate, result)
    };
    state.stage = Stage::Begin;
    queued(flight, state, op, job)
}
fn write_next(flight: Flight, mut state: State) -> Progress
{
    let plan = state.plan.as_ref().unwrap();
    if state.written == plan.data.len()
    {
        state.stage = Stage::Finish;
        let job = submit_args(flight.provider,
                              Operation::FinishWrite,
                              Arguments { volume: state.volume,
                                          token: state.handle,
                                          ..Default::default() });
        queued(flight, state, Operation::FinishWrite, job)
    }
    else
    {
        let end = (state.written + fs::MAX_TRANSFER).min(plan.data.len());
        let job = submit_args(flight.provider,
                              Operation::WriteChunk,
                              Arguments { volume: state.volume,
                                          token: state.handle,
                                          payload: &plan.data[state.written..end],
                                          ..Default::default() });
        state.written = end;
        state.stage = Stage::Chunk;
        queued(flight, state, Operation::WriteChunk, job)
    }
}
pub(super) fn advance(mut flight: Flight, result: Result<Vec<u8>, Status>, never_sent: bool)
                      -> Progress
{
    let Step::Write(mut state) = core::mem::replace(&mut flight.step, Step::Count)
    else
    {
        return Progress::Done(Err(BAD_ABI));
    };
    if state.stage == Stage::Abort
    {
        let outcome = match result {
            Ok(bytes) => fs::AbortReceipt::decode(&bytes).map_err(|_|BAD_ABI).and_then(|receipt| {
                receipt.0.map(|receipt|files(|manager|manager.publish_provider_write(state.handle,receipt))).transpose()
            }),
            Err(NOT_FOUND) if !state.may_publish => Ok(None),
            Err(error) => Err(error)
        };
        return match outcome
        {
            Ok(Some(entry)) =>
            {
                Progress::Done(Ok(if flight.key.operation == ABORT_DATA_WRITE
                                  {
                                      Reply::Unmounted
                                  }
                                  else
                                  {
                                      Reply::Entry(entry)
                                  }))
            }
            Ok(None) =>
            {
                let result =
                    if state.error == OK { Ok(Reply::Unmounted) } else { Err(state.error) };
                terminal(state, result)
            }
            Err(_) =>
            {
                quarantine(state.handle);
                Progress::Done(Err(ROLLBACK_FAILED))
            }
        };
    }
    if state.stage == Stage::Finish
    {
        if never_sent || matches!(result, Err(OPERATION_CANCELLED))
        {
            state.may_publish = false;
        }
        let published = result.and_then(|bytes| fs::WriteReceipt::decode(&bytes).map_err(|_|BAD_ABI)
                        .and_then(|receipt|files(|manager|manager.publish_provider_write(state.handle,receipt))));
        return match published
        {
            Ok(entry) => Progress::Done(Ok(Reply::Entry(entry))),
            Err(error) => failed(flight, state, error)
        };
    }
    if state.stage == Stage::Begin && never_sent
    {
        state.remote = false;
    }
    if flight.abandoned
    {
        return failed(flight, state, OPERATION_CANCELLED);
    }
    match state.stage
    {
        Stage::Parent =>
        {
            let plan = state.plan.as_ref().unwrap();
            let checked =
                result.and_then(|bytes| {
                          let entry =
                              super::super::catalog(state.volume, &bytes, Some(plan.parent), None)?;
                          if !entry.is_directory()
                          {
                              Err(BAD_ARGUMENT)
                          }
                          else if entry.flags & 1 != 0
                          {
                              Err(ACCESS_DENIED)
                          }
                          else
                          {
                              Ok(())
                          }
                      });
            if let Err(error) = checked
            {
                return failed(flight, state, error);
            }
            let job = submit_args(flight.provider,
                                  Operation::Lookup,
                                  Arguments { volume: state.volume,
                                              parent: plan.parent,
                                              payload: &plan.name,
                                              ..Default::default() });
            state.stage = Stage::Lookup;
            queued(flight, state, Operation::Lookup, job)
        }
        Stage::Lookup =>
        {
            let expected = state.plan.as_ref().unwrap().expected;
            let checked = match result
            {
                Ok(bytes) => files(|manager| manager.select_provider_target(state.handle, &bytes)),
                Err(NOT_FOUND) if expected == 0 => Ok(()),
                Err(error) => Err(error)
            };
            if let Err(error) = checked
            {
                failed(flight, state, error)
            }
            else
            {
                begin(flight, state)
            }
        }
        Stage::Begin =>
        {
            let checked =
                result.and_then(|bytes| {
                          files(|manager| manager.confirm_provider_write(state.handle, &bytes))
                      });
            if let Err(error) = checked
            {
                failed(flight, state, error)
            }
            else
            {
                write_next(flight, state)
            }
        }
        Stage::Chunk =>
        {
            let checked =
                result.and_then(|bytes| if bytes.is_empty() { Ok(()) } else { Err(BAD_ABI) });
            if let Err(error) = checked
            {
                failed(flight, state, error)
            }
            else
            {
                write_next(flight, state)
            }
        }
        Stage::Finish | Stage::Abort => unreachable!()
    }
}
fn key(operation: u32, handle: u32, volume: u32) -> Key
{
    Key { record: Record::Write,
          operation,
          volume,
          index: 0,
          cnid: 0,
          handle,
          fork: 0,
          capacity: 0,
          cursor: DirectoryScanCursor::default(),
          resource: None }
}
pub(in crate::toolbox::files) fn request(owner: u64, call: &mut DataWriteCall) -> Status
{
    let thread = crate::context::current_thread().0;
    if thread <= 1
    {
        return WRONG_THREAD;
    }
    let key = key(call.operation, call.handle, 0);
    match claim(owner, thread, key, "")
    {
        Ok(Some(flight)) =>
        {
            return match super::advance(flight)
            {
                Progress::Done(Ok(Reply::Entry(entry))) =>
                {
                    call.entry = entry;
                    OK
                }
                Progress::Done(Ok(Reply::Unmounted)) => OK,
                Progress::Done(Err(error)) => error,
                Progress::Done(_) => BAD_ABI,
                Progress::Pending(flight) => waiting(flight)
            };
        }
        Ok(None) => (),
        Err(error) => return error
    }
    let (provider, state, job) = if call.operation == COMMIT_DATA_WRITE
    {
        let plan = match files(|manager| manager.prepare_provider_write(owner, call.handle))
        {
            Ok(plan) => plan,
            Err(error) => return error
        };
        let job = submit_args(plan.provider,
                              Operation::Catalog,
                              Arguments { volume: plan.volume,
                                          node: plan.parent,
                                          ..Default::default() });
        let state = State { handle:      plan.handle,
                            volume:      plan.volume,
                            stage:       Stage::Parent,
                            written:     0,
                            remote:      false,
                            may_publish: false,
                            error:       OK,
                            plan:        Some(plan) };
        (state.plan.as_ref().unwrap().provider, state, job)
    }
    else if call.operation == ABORT_DATA_WRITE
    {
        let (provider, volume) =
            match files(|manager| manager.cleanup_provider_write(owner, call.handle))
            {
                Ok(value) => value,
                Err(error) => return error
            };
        let job = submit_args(provider,
                              Operation::AbortWrite,
                              Arguments { volume,
                                          token: call.handle,
                                          ..Default::default() });
        (provider,
         State { plan: None,
                 handle: call.handle,
                 volume,
                 stage: Stage::Abort,
                 written: 0,
                 remote: true,
                 may_publish: true,
                 error: OK },
         job)
    }
    else
    {
        return BAD_ARGUMENT;
    };
    let job = match job
    {
        Ok(job) => job,
        Err(error) =>
        {
            if state.remote
            {
                quarantine(state.handle);
            }
            else
            {
                let _ = retire(state.handle);
            }
            return error;
        }
    };
    waiting(Flight { thread,
                     owner,
                     key,
                     provider,
                     step: Step::Write(state),
                     job,
                     abandoned: false,
                     input: String::new(),
                     path: None })
}
pub(super) fn pump_cleanup()
{
    if !WRITE_CLEANUP.load(Ordering::Acquire)
    {
        return;
    }
    if !WRITE_CLEANUP.swap(false, Ordering::AcqRel)
    {
        return;
    }
    let mut flights = FLIGHTS.lock();
    if flights.len() == MAX_FLIGHTS
    {
        WRITE_CLEANUP.store(true, Ordering::Release);
        return;
    }
    if flights.capacity() < MAX_FLIGHTS
    {
        let additional = MAX_FLIGHTS - flights.len();
        if flights.try_reserve_exact(additional).is_err()
        {
            WRITE_CLEANUP.store(true, Ordering::Release);
            return;
        }
    }
    drop(flights);
    let (handle, provider, volume) = match files(|manager| Ok(manager.next_orphaned_write()))
    {
        Ok(Some(value)) => value,
        Ok(None) => return,
        Err(_) =>
        {
            WRITE_CLEANUP.store(true, Ordering::Release);
            return;
        }
    };
    WRITE_CLEANUP.store(true, Ordering::Release);
    match submit_args(provider,
                      Operation::AbortWrite,
                      Arguments { volume,
                                  token: handle,
                                  ..Default::default() })
    {
        Ok(job) =>
        {
            FLIGHTS.lock().push(Flight { thread: 0,
                                         owner: 0,
                                         key: key(ABORT_DATA_WRITE, handle, volume),
                                         provider,
                                         step: Step::Write(State { plan: None,
                                                                   handle,
                                                                   volume,
                                                                   stage: Stage::Abort,
                                                                   written: 0,
                                                                   remote: true,
                                                                   may_publish: true,
                                                                   error:
                                                                       OPERATION_CANCELLED }),
                                         job,
                                         abandoned: true,
                                         input: String::new(),
                                         path: None });
            REAP.store(true, Ordering::Release);
        }
        Err(_) => quarantine(handle)
    }
}
