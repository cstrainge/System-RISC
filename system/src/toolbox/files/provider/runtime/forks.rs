//! Owned provider fork I/O and deferred app-exit cleanup.
#![forbid(unsafe_code)]
use super::*;

#[derive(Clone, Copy)]
pub(super) enum Action
{
    Open
    {
        handle: u32
    },
    Read
    {
        handle: u32, offset: u64, count: u64
    },
    Close
    {
        handle: u32
    },
    Cleanup
    {
        handle: u32, error: Status
    }
}
pub(in crate::toolbox::files::provider) fn request_fork_cleanup()
{
    FORK_CLEANUP.store(true, Ordering::Release);
}
fn retire(handle: u32) -> Result<Reply, Status>
{
    files(|manager| manager.retire_provider_file(handle)).map(|()| Reply::Unmounted)
}
fn quarantine(handle: u32)
{
    let _ = files(|manager| manager.quarantine_provider_file(handle));
}
fn cleanup(mut flight: Flight, handle: u32, error: Status) -> Progress
{
    let job = files(|manager| manager.cleanup_provider_file(handle)).and_then(|volume| {
                                                                        submit_args(flight.provider,
                              Operation::Close,
                              Arguments { volume,
                                          token: handle,
                                          ..Default::default() })
                                                                    });
    match job
    {
        Ok(job) =>
        {
            flight.job = job;
            flight.step = Step::File(Action::Cleanup { handle,
                                                       error });
            Progress::Pending(flight)
        }
        Err(_) =>
        {
            quarantine(handle);
            Progress::Done(Err(ROLLBACK_FAILED))
        }
    }
}
fn acknowledged(result: &Result<Vec<u8>, Status>) -> bool
{
    matches!(result, Ok(bytes) if bytes.is_empty()) || matches!(result, Err(NOT_FOUND))
}
pub(super) fn advance(flight: Flight, result: Result<Vec<u8>, Status>, never_sent: bool)
                      -> Progress
{
    let Step::File(action) = flight.step
    else
    {
        return Progress::Done(Err(BAD_ABI));
    };
    match action
    {
        Action::Open { handle } =>
        {
            if never_sent
            {
                return Progress::Done(retire(handle).and(Err(OPERATION_CANCELLED)));
            }
            let opened = if flight.abandoned
            {
                Err(OPERATION_CANCELLED)
            }
            else
            {
                result.and_then(|bytes| {
                          files(|manager| manager.publish_provider_file(handle, &bytes))
                      })
            };
            match opened
            {
                Ok(length) => Progress::Done(Ok(Reply::Opened(handle, length))),
                Err(error) => cleanup(flight, handle, error)
            }
        }
        Action::Read { handle,
                       offset,
                       count } =>
        {
            let delivered =
                if flight.abandoned { None } else { result.as_ref().ok().map(Vec::len) };
            let completed = files(|manager| {
                manager.finish_provider_file_read(handle, offset, count, delivered)
            });
            let result = completed.and_then(|(position, length)| {
                                      result.map(|bytes| {
                                                Reply::Read { bytes,
                                                              position,
                                                              length }
                                            })
                                  });
            Progress::Done(result)
        }
        Action::Close { handle } =>
        {
            if acknowledged(&result)
            {
                Progress::Done(retire(handle))
            }
            else
            {
                quarantine(handle);
                Progress::Done(Err(result.err().unwrap_or(BAD_ABI)))
            }
        }
        Action::Cleanup { handle,
                          error } =>
        {
            if acknowledged(&result)
            {
                Progress::Done(retire(handle).and(Err(error)))
            }
            else
            {
                quarantine(handle);
                Progress::Done(Err(ROLLBACK_FAILED))
            }
        }
    }
}
fn prepare(owner: u64, call: &FileCall, capacity: usize)
           -> Result<(u32, Action, Operation, Arguments<'static>), Status>
{
    match call.operation
    {
        OPEN =>
        {
            let (provider, handle) = files(|manager| {
                manager.reserve_provider_file(owner, call.volume, call.cnid, call.fork)
            })?;
            Ok((provider,
                Action::Open { handle },
                Operation::Open,
                Arguments { volume: call.volume,
                            node: call.cnid,
                            token: handle,
                            ..Default::default() }))
        }
        READ =>
        {
            let (provider, args) =
                files(|manager| manager.begin_provider_file_read(owner, call.handle, capacity))?;
            Ok((provider,
                Action::Read { handle: call.handle,
                               offset: args.offset,
                               count:  args.length },
                Operation::Read,
                args))
        }
        CLOSE =>
        {
            let (provider, volume) =
                files(|manager| manager.begin_provider_file_close(owner, call.handle))?;
            Ok((provider,
                Action::Close { handle: call.handle },
                Operation::Close,
                Arguments { volume,
                            token: call.handle,
                            ..Default::default() }))
        }
        _ => Err(BAD_ARGUMENT)
    }
}
pub(super) fn start(owner: u64, thread: u64, key: Key, call: &mut FileCall, output: &mut [u8])
                    -> Status
{
    let (provider, action, operation, args) = match prepare(owner, call, output.len())
    {
        Ok(prepared) => prepared,
        Err(error) => return error
    };
    if let Action::Read { handle,
                          offset,
                          count: 0 } = action
    {
        let result = files(|manager| manager.finish_provider_file_read(handle, offset, 0, Some(0)))
            .map(|(position, length)| Reply::Read { bytes: Vec::new(), position, length });
        return reply(call, output, result);
    }
    let job = match submit_args(provider, operation, args)
    {
        Ok(job) => job,
        Err(error) =>
        {
            match action
            {
                Action::Open { handle,
                               .. } =>
                {
                    let _ = retire(handle);
                }
                Action::Read { handle,
                               offset,
                               count } =>
                {
                    let _ = files(|manager| {
                        manager.finish_provider_file_read(handle, offset, count, None)
                    });
                }
                Action::Close { handle } => quarantine(handle),
                Action::Cleanup { .. } => ()
            }
            return error;
        }
    };
    waiting(Flight { thread,
                     owner,
                     key,
                     provider,
                     step: Step::File(action),
                     job,
                     abandoned: false,
                     input: String::new(),
                     path: None })
}
pub(super) fn pump_cleanup()
{
    if !FORK_CLEANUP.load(Ordering::Acquire)
    {
        return;
    }
    if !FORK_CLEANUP.swap(false, Ordering::AcqRel)
    {
        return;
    }
    let mut flights = FLIGHTS.lock();
    if flights.len() == MAX_FLIGHTS
    {
        FORK_CLEANUP.store(true, Ordering::Release);
        return;
    }
    if flights.capacity() < MAX_FLIGHTS
    {
        let additional = MAX_FLIGHTS - flights.len();
        if flights.try_reserve_exact(additional).is_err()
        {
            FORK_CLEANUP.store(true, Ordering::Release);
            return;
        }
    }
    drop(flights);
    let next = files(|manager| manager.next_orphaned_file());
    let (handle, provider, volume) = match next
    {
        Ok(Some(next)) => next,
        Ok(None) => return,
        Err(_) =>
        {
            FORK_CLEANUP.store(true, Ordering::Release);
            return;
        }
    };
    // Check another mark next time. A concurrent signal cannot be cleared by
    // the empty-queue observation above (the flag was consumed before querying).
    FORK_CLEANUP.store(true, Ordering::Release);
    match submit_args(provider,
                      Operation::Close,
                      Arguments { volume,
                                  token: handle,
                                  ..Default::default() })
    {
        Ok(job) =>
        {
            let key = Key { operation: CLOSE,
                            volume,
                            cnid: 0,
                            index: 0,
                            handle,
                            fork: 0,
                            capacity: 0 };
            let step = Step::File(Action::Cleanup { handle,
                                                    error: OPERATION_CANCELLED });
            FLIGHTS.lock().push(Flight { thread: 0,
                                         owner: 0,
                                         key,
                                         provider,
                                         step,
                                         job,
                                         abandoned: true,
                                         input: String::new(),
                                         path: None });
            REAP.store(true, Ordering::Release);
        }
        Err(_) => quarantine(handle)
    }
}
