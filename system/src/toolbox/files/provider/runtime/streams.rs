//! One owned provider operation per trap, with acknowledged rollback on every
//! failure. Payload snapshots belong to jobs; no userspace pointer survives park.
use super::*;
pub(super) struct State
{
    handle:      u64,
    operation:   u32,
    fork:        u32,
    count:       usize,
    properties:  Vec<u8>,
    aborting:    bool,
    may_publish: bool,
    error:       Status
}
fn reply(handle: u64, completed: bool, entry: CatalogInfo, bytes: Vec<u8>)
         -> Result<Reply, Status>
{
    Ok(Reply::Stream { handle,
                       completed,
                       entry,
                       bytes })
}
fn retire(handle: u64) -> Result<(), Status>
{
    files(|manager| manager.retire_remote_stream(handle))
}
fn quarantine(handle: u64, error: Status, stage: &str)
{
    serial!("FILE_STREAM_QUARANTINED handle={handle} stage={stage} status={error}; staged file, \
             I/O subject and cleanup guard remain retained");
    let _ = files(|manager| {
        manager.quarantine_remote_stream(handle);
        Ok(())
    });
}
fn failed(mut flight: Flight, mut state: State, error: Status) -> Progress
{
    state.aborting = true;
    state.error = error;
    let volume = match files(|m| Ok(m.streams.completing(state.handle).ok_or(NOT_FOUND)?.volume))
    {
        Ok(v) => v,
        Err(error) => return Progress::Done(Err(error))
    };
    match submit_args(flight.job.subject(),
                      flight.provider,
                      Operation::AbortWrite,
                      Arguments { volume,
                                  token: state.handle,
                                  ..Default::default() })
    {
        Ok(job) =>
        {
            flight.job = job;
            flight.step = Step::Stream(state);
            Progress::Pending(flight)
        }
        Err(error) =>
        {
            quarantine(state.handle, error, "submit rollback");
            Progress::Done(Err(ROLLBACK_FAILED))
        }
    }
}
pub(super) fn advance(mut flight: Flight, result: Result<Vec<u8>, Status>, never_sent: bool)
                      -> Progress
{
    let Step::Stream(state) = core::mem::replace(&mut flight.step, Step::Count)
    else
    {
        return Progress::Done(Err(BAD_ABI));
    };
    if state.aborting
    {
        let outcome=match result
        {
            Ok(bytes)=>fs::AbortReceipt::decode(&bytes).map_err(|_|BAD_ABI)
                .and_then(|r|r.0.map(|r|files(|m|m.publish_remote_stream(state.handle,r))).transpose()),
            Err(NOT_FOUND) if !state.may_publish=>Ok(None),
            Err(error)=>Err(error)
        };
        return match outcome
        {
            Ok(Some(entry)) => Progress::Done(reply(state.handle, true, entry, Vec::new())),
            Ok(None) =>
            {
                Progress::Done(retire(state.handle).and_then(|()| {
                                                       if state.error == OK
                                                       {
                                                           reply(state.handle,
                                                                 true,
                                                                 CatalogInfo::default(),
                                                                 Vec::new())
                                                       }
                                                       else
                                                       {
                                                           Err(state.error)
                                                       }
                                                   }))
            }
            Err(error) =>
            {
                quarantine(state.handle, error, "acknowledge rollback");
                Progress::Done(Err(ROLLBACK_FAILED))
            }
        };
    }
    if never_sent && state.operation == BEGIN_FORK_WRITE
    {
        return Progress::Done(retire(state.handle).and(Err(OPERATION_CANCELLED)));
    }
    let bytes = match result
    {
        Ok(bytes) => bytes,
        Err(error) => return failed(flight, state, error)
    };
    let outcome = (|| {
        match state.operation
        {
            BEGIN_FORK_WRITE =>
            {
                let properties =
                    fs::FileProperties::decode(&state.properties).map_err(|_| BAD_ABI)?;
                files(|m| m.remote_stream_begin(state.handle, &bytes, properties))?;
                reply(state.handle, false, CatalogInfo::default(), Vec::new())
            }
            APPEND_FORK_WRITE =>
            {
                if !bytes.is_empty()
                {
                    return Err(BAD_ABI);
                }
                files(|m| m.remote_stream_ready(state.handle, state.fork, state.count))?;
                reply(state.handle, false, CatalogInfo::default(), Vec::new())
            }
            READ_FORK_WRITE =>
            {
                if bytes.len() != state.count
                {
                    return Err(BAD_ABI);
                }
                files(|m| m.remote_stream_ready(state.handle, state.fork, 0))?;
                reply(state.handle, false, CatalogInfo::default(), bytes)
            }
            FINISH_FORK_WRITE =>
            {
                let receipt = fs::WriteReceipt::decode(&bytes).map_err(|_| BAD_ABI)?;
                let entry = files(|m| m.publish_remote_stream(state.handle, receipt))?;
                reply(state.handle, true, entry, Vec::new())
            }
            _ => Err(BAD_ABI)
        }
    })();
    match outcome
    {
        Ok(result) if !flight.abandoned || state.operation == FINISH_FORK_WRITE =>
        {
            Progress::Done(Ok(result))
        }
        Ok(_) => failed(flight, state, OPERATION_CANCELLED),
        Err(error) => failed(flight, state, error)
    }
}
fn deliver(call: &mut ForkWriteCall, output: &mut [u8], progress: Progress) -> Status
{
    match progress
    {
        Progress::Pending(flight) => waiting(flight),
        Progress::Done(Ok(Reply::Stream { handle,
                                          completed,
                                          entry,
                                          bytes })) =>
        {
            if bytes.len() > output.len()
            {
                return BAD_ABI;
            }
            output[..bytes.len()].copy_from_slice(&bytes);
            call.transferred = bytes.len();
            call.handle = handle;
            call.completed = u32::from(completed);
            call.entry = entry;
            OK
        }
        Progress::Done(Err(error)) => error,
        _ => BAD_ABI
    }
}
pub(crate) fn request(owner: u64, call: &mut ForkWriteCall, name: &str, comment: &str,
                      data: &[u8], output: &mut [u8])
                      -> Status
{
    let thread = crate::context::current_thread().0;
    if thread <= 1
    {
        return WRONG_THREAD;
    }
    if call.operation == ABANDON_FORK_WRITE
    {
        return files(|m| m.stream_request(owner, call, name, comment, data, output)).err()
                                                                                    .unwrap_or(OK);
    }
    let key = Key { record:    Record::Stream,
                    operation: call.operation,
                    volume:    call.volume,
                    cnid:      call.expected_cnid,
                    index:     call.parent,
                    handle:    call.handle,
                    fork:      call.fork,
                    capacity:  call.count.max(call.capacity),
                    cursor:    DirectoryScanCursor::default(),
                    resource:  None,
                    when:      call.when,
                    metadata:  Some(super::super::super::properties::finder_key(call.finder_info)) };
    match claim(owner, thread, key, name)
    {
        Ok(Some(flight)) => return deliver(call, output, super::advance(flight)),
        Ok(None) => (),
        Err(error) => return error
    }
    match begin(owner, thread, key, call, name, comment, data)
    {
        Ok(flight) => waiting(flight),
        Err(error) => error
    }
}

pub(super) fn begin<'a>(owner: impl Into<crate::toolbox::files::subject::Access<'a>>, thread: u64,
                        key: Key, call: &ForkWriteCall, name: &str, comment: &str, data: &[u8])
                        -> Result<Flight, Status>
{
    let owner = owner.into();
    (|| {
        let mut input = String::new();
        input.try_reserve_exact(name.len()).map_err(|_| NO_MEMORY)?;
        input.push_str(name);
        let mut properties = Vec::new();
        let (provider, handle, op, args, count) = if call.operation == BEGIN_FORK_WRITE
        {
            let count = fs::PROPERTIES_BYTES + name.len() + comment.len();
            properties.try_reserve_exact(count).map_err(|_| NO_MEMORY)?;
            properties.resize(count, 0);
            fs::FileProperties { name,
                                 comment,
                                 created: call.created,
                                 modified: call.modified,
                                 backup: (call.backup_present != 0).then_some(call.backup),
                                 attributes: call.attributes,
                                 finder_info: call.finder_info }.encode(&mut properties)
                                                                .map_err(|_| BAD_ARGUMENT)?;
            let authority = owner.authority()?;
            let (provider, handle) =
                files(|m| m.reserve_remote_stream_authorized(owner, &authority, call, name))?;
            let op = if call.expected_cnid == 0
            {
                Operation::BeginCreate
            }
            else
            {
                Operation::BeginReplace
            };
            let args = Arguments { volume: call.volume,
                                   token: handle,
                                   parent: if call.expected_cnid == 0 { call.parent } else { 0 },
                                   node: call.expected_cnid,
                                   flags: if call.expected_cnid == 0
                                   {
                                       0
                                   }
                                   else
                                   {
                                       fs::REPLACE_PROPERTIES
                                   },
                                   length: call.data_length,
                                   auxiliary: call.resource_length,
                                   when: call.when,
                                   ..Default::default() };
            (provider, handle, op, args, 0)
        }
        else
        {
            let (provider, args) = files(|m| m.remote_stream_action(owner, call))?;
            let op = match call.operation
            {
                APPEND_FORK_WRITE => Operation::WriteChunk,
                READ_FORK_WRITE => Operation::ReadWrite,
                FINISH_FORK_WRITE => Operation::FinishWrite,
                ABORT_FORK_WRITE => Operation::AbortWrite,
                _ => return Err(BAD_ARGUMENT)
            };
            let count = if call.operation == READ_FORK_WRITE
            {
                files(|m| {
                    let mark = m.streams.get(call.handle).ok_or(NOT_FOUND)?;
                    let progress = mark.progress.lock();
                    let remote = progress.remote.as_ref().ok_or(BAD_ABI)?;
                    Ok((remote.written[call.fork as usize] - call.offset).min(call.capacity as u64)
                       as usize)
                })?
            }
            else
            {
                data.len()
            };
            let args = if matches!(op, Operation::FinishWrite | Operation::AbortWrite)
            {
                Arguments { volume: args.volume,
                            token: args.token,
                            ..Default::default() }
            }
            else
            {
                args
            };
            (provider, call.handle, op, args, count)
        };
        let payload = if call.operation == BEGIN_FORK_WRITE { properties.as_slice() } else { data };
        let submitted = submit_object(IoObject::Stream(handle),
                                      provider,
                                      op,
                                      Arguments { payload,
                                                  ..args });
        let job = match submitted
        {
            Ok(job) => job,
            Err(error) =>
            {
                if call.operation == BEGIN_FORK_WRITE
                {
                    let _ = retire(handle);
                }
                else
                {
                    let _ = files(|m| m.remote_stream_ready(handle, call.fork, 0));
                }
                return Err(error);
            }
        };
        Ok(Flight { thread,
                    owner: owner.owned(),
                    key,
                    provider,
                    job,
                    input,
                    path: None,
                    abandoned: false,
                    step: Step::Stream(State { handle,
                                               operation: call.operation,
                                               fork: call.fork,
                                               count,
                                               properties,
                                               aborting: call.operation == ABORT_FORK_WRITE,
                                               may_publish: call.operation
                                                            == FINISH_FORK_WRITE,
                                               error: OK }) })
    })()
}
pub(super) fn pump_cleanup()
{
    if super::super::super::streams::SCOPES.pending()
    {
        if files(|manager| {
               super::super::super::streams::SCOPES.observe(&mut manager.streams);
               Ok(())
           }).is_err()
        {
            return;
        }
    }

    // Reserve operation-list storage before acquiring or changing ownership.
    {
        let mut flights = FLIGHTS.lock();
        if reserve_flight_storage(&mut flights).is_err()
        {
            return;
        }
    }
    let prepared = files(|m| {
        for (handle, mark) in m.streams.iter()
        {
            let mut progress = mark.progress.lock();
            if !progress.abandoned || progress.quarantined || progress.closed
            {
                continue;
            }
            let Some(remote) = progress.remote.as_mut()
            else
            {
                continue;
            };
            if remote.running
            {
                continue;
            }
            remote.running = true;
            return Ok(Some((handle,
                            mark.volume,
                            remote.provider,
                            mark.authority.flight_subject(mark.owner))));
        }
        Ok(None)
    });
    let Ok(Some((handle, volume, provider, owner))) = prepared
    else
    {
        return;
    };
    match submit_object(IoObject::Stream(handle),
                        provider,
                        Operation::AbortWrite,
                        Arguments { volume,
                                    token: handle,
                                    ..Default::default() })
    {
        Ok(job) =>
        {
            let key = Key { record: Record::Stream,
                            operation: ABORT_FORK_WRITE,
                            volume,
                            index: 0,
                            cnid: 0,
                            handle,
                            fork: 0,
                            capacity: 0,
                            cursor: DirectoryScanCursor::default(),
                            resource: None,
                            when: 0,
                            metadata: None };
            FLIGHTS.lock().push(Flight { thread: 0,
                                         owner,
                                         key,
                                         provider,
                                         job,
                                         input: String::new(),
                                         path: None,
                                         abandoned: true,
                                         step: Step::Stream(State { handle,
                                                                    operation:
                                                                        ABORT_FORK_WRITE,
                                                                    fork: 0,
                                                                    count: 0,
                                                                    properties: Vec::new(),
                                                                    aborting: true,
                                                                    may_publish: true,
                                                                    error: OK }) });
            REAP.store(true, Ordering::Release);
        }
        Err(error) => quarantine(handle, error, "submit exit cleanup")
    }
}
