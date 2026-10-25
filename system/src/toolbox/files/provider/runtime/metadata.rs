//! Native and legacy metadata changes use the same owned provider transaction.
#![forbid(unsafe_code)]
use super::*;
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage
{
    Begin,
    Finish,
    Abort
}
pub(super) struct State
{
    handle:      u32,
    volume:      u32,
    stage:       Stage,
    may_publish: bool,
    error:       Status
}
pub(crate) fn pending() -> bool
{
    let thread = crate::context::current_thread().0;
    FLIGHTS.lock().iter().any(|f| f.thread == thread && f.key.record == Record::Metadata)
}
fn retire(handle: u32) -> Result<(), Status>
{
    files(|m| m.retire_provider_write(handle))
}
fn quarantine(handle: u32)
{
    let _ = files(|m| m.quarantine_provider_write(handle));
}
fn failed(mut flight: Flight, mut state: State, error: Status) -> Progress
{
    state.stage = Stage::Abort;
    state.error = error;
    match submit_args(flight.provider,
                      Operation::AbortWrite,
                      Arguments { volume: state.volume,
                                  token: state.handle,
                                  ..Default::default() })
    {
        Ok(job) =>
        {
            flight.job = job;
            flight.step = Step::Metadata(state);
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
    let Step::Metadata(mut state) = core::mem::replace(&mut flight.step, Step::Count)
    else
    {
        return Progress::Done(Err(BAD_ABI));
    };
    if state.stage == Stage::Abort
    {
        let outcome = match result
        {
            Ok(bytes) => writes::acknowledge_abort(state.handle, &bytes),
            Err(NOT_FOUND) if !state.may_publish => Ok(None),
            Err(error) => Err(error)
        };
        return match outcome
        {
            Ok(Some(entry)) => Progress::Done(Ok(Reply::Entry(entry))),
            Ok(None) => Progress::Done(retire(state.handle).and(Err(state.error))),
            Err(_) =>
            {
                quarantine(state.handle);
                Progress::Done(Err(ROLLBACK_FAILED))
            }
        };
    }
    if state.stage == Stage::Begin
    {
        if never_sent
        {
            return Progress::Done(retire(state.handle).and(Err(OPERATION_CANCELLED)));
        }
        let begun =
            result.and_then(|bytes| {
                      let tag = u32::from_le_bytes(bytes.get(..4)
                                                        .ok_or(BAD_ABI)?
                                                        .try_into()
                                                        .map_err(|_| BAD_ABI)?);
                      if tag > 1
                      {
                          return Err(BAD_ABI);
                      }
                      let entry = files(|m| {
                          m.confirm_provider_metadata(state.handle, &bytes[4..], &flight.input)
                      })?;
                      Ok((tag, entry))
                  });
        match begun
        {
            Ok((0, entry)) =>
            {
                return Progress::Done(retire(state.handle).map(|()| Reply::Entry(entry)));
            }
            Ok(_) if flight.abandoned => return failed(flight, state, OPERATION_CANCELLED),
            Ok(_) => (),
            Err(error) => return failed(flight, state, error)
        }
        match submit_args(flight.provider,
                          Operation::FinishWrite,
                          Arguments { volume: state.volume,
                                      token: state.handle,
                                      ..Default::default() })
        {
            Ok(job) =>
            {
                state.stage = Stage::Finish;
                state.may_publish = true;
                flight.job = job;
                flight.step = Step::Metadata(state);
                Progress::Pending(flight)
            }
            Err(error) => failed(flight, state, error)
        }
    }
    else
    {
        if never_sent
        {
            state.may_publish = false;
        }
        let completed =
            result.and_then(|bytes| {
                if flight.key.operation == REMOVE
                {
                    let receipt = fs::RemovalReceipt::decode(&bytes).map_err(|_| BAD_ABI)?;
                    files(|m| m.publish_provider_removal(state.handle, receipt))?.ok_or(BAD_ABI)
                }
                else
                {
                    fs::MetadataReceipt::decode(&bytes).map_err(|_| BAD_ABI)
                    .and_then(|r| files(|m| m.publish_provider_write(state.handle, r.0)))
                }
            });
        match completed
        {
            Ok(entry) => Progress::Done(Ok(Reply::Entry(entry))),
            Err(error) => failed(flight, state, error)
        }
    }
}
pub(crate) fn request(owner: u64, call: &mut FileCall, text: &str) -> Status
{
    let thread = crate::context::current_thread().0;
    if thread <= 1
    {
        return WRONG_THREAD;
    }
    let key = Key { record:    Record::Metadata,
                    operation: call.operation,
                    volume:    call.volume,
                    cnid:      call.cnid,
                    index:     call.index,
                    handle:    0,
                    fork:      call.fork,
                    capacity:  0,
                    cursor:    DirectoryScanCursor::default(),
                    resource:  None,
                    when:      call.position,
                    metadata:  super::super::super::properties::key(call) };
    match claim(owner, thread, key, text)
    {
        Ok(Some(flight)) => return deliver(call, super::advance(flight)),
        Ok(None) => (),
        Err(error) => return error
    }
    match begin(owner, thread, key, call, text)
    {
        Ok(flight) => waiting(flight),
        Err(error) => error
    }
}

pub(super) fn begin(owner: u64, thread: u64, key: Key, call: &FileCall, text: &str)
                    -> Result<Flight, Status>
{
    (|| {
        let mut patch = fs::MetadataPatch::default();
        match call.operation
        {
            super::super::super::properties::RESTORE_MODIFIED => patch.modified = Some(call.length),
            SET_COMMENT => patch.comment = Some(text),
            SET_PROPERTIES => patch = super::super::super::properties::patch(call, text)?,
            SET_LOCKED =>
            {
                patch.attributes_mask = 1;
                patch.attributes = call.index as u16;
            }
            SET_STATIONERY =>
            {
                patch.finder_mask[8] = 8;
                patch.finder_info[8] = if call.index != 0 { 8 } else { 0 };
            }
            SET_ICON_LOCATION =>
            {
                patch.finder_mask[8] = 1;
                patch.finder_info[8] = call.fork as u8;
                patch.finder_mask[10..14].fill(255);
                let (h, v) = if call.fork == 0
                {
                    (-1i16, -1i16)
                }
                else
                {
                    (call.index as u16 as i16, (call.index >> 16) as u16 as i16)
                };
                patch.finder_info[10..12].copy_from_slice(&v.to_be_bytes());
                patch.finder_info[12..14].copy_from_slice(&h.to_be_bytes());
            }
            RENAME | MOVE_INTO | REMOVE => (),
            SET_FOLDER_STATE =>
            {
                patch.finder_mask[..8].fill(255);
                patch.finder_mask[14..20].fill(255);
                patch.finder_info = call.entry.finder_info;
            }
            _ => return Err(BAD_ARGUMENT)
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(fs::MAX_PATCH_BYTES).map_err(|_| NO_MEMORY)?;
        bytes.resize(fs::MAX_PATCH_BYTES, 0);
        let n = if call.operation == RENAME
        {
            if text.len() > bytes.len()
            {
                bytes.try_reserve_exact(text.len() - bytes.len()).map_err(|_| NO_MEMORY)?;
                bytes.resize(text.len(), 0);
            }
            bytes[..text.len()].copy_from_slice(text.as_bytes());
            text.len()
        }
        else if matches!(call.operation, MOVE_INTO | REMOVE)
        {
            0
        }
        else
        {
            patch.encode(&mut bytes).map_err(|_| BAD_ARGUMENT)?
        };
        bytes.truncate(n);
        let mut input = String::new();
        input.try_reserve_exact(text.len()).map_err(|_| NO_MEMORY)?;
        input.push_str(text);
        let authority = crate::toolbox::files::creation_authority(owner)?;
        let (provider, handle) =
            files(|m| m.reserve_provider_metadata_for(owner, &authority, call))?;
        let job = match submit_args(provider,
                                    match call.operation
                                    {
                                        RENAME => Operation::Rename,
                                        REMOVE => Operation::Remove,
                                        MOVE_INTO => Operation::Move,
                                        _ => Operation::SetMetadata
                                    },
                                    Arguments { volume: call.volume,
                                                node: call.cnid,
                                                parent: if call.operation == MOVE_INTO
                                                {
                                                    call.index
                                                }
                                                else
                                                {
                                                    0
                                                },
                                                token: handle,
                                                when: call.position,
                                                payload: &bytes,
                                                ..Default::default() })
        {
            Ok(job) => job,
            Err(error) =>
            {
                retire(handle)?;
                return Err(error);
            }
        };
        Ok::<_, Status>(Flight { thread,
                                 owner,
                                 key,
                                 provider,
                                 job,
                                 abandoned: false,
                                 input,
                                 path: None,
                                 step: Step::Metadata(State { handle,
                                                              volume: call.volume,
                                                              stage: Stage::Begin,
                                                              may_publish: false,
                                                              error: OK }) })
    })()
}
fn deliver(call: &mut FileCall, progress: Progress) -> Status
{
    match progress
    {
        Progress::Pending(flight) => waiting(flight),
        Progress::Done(Ok(Reply::Entry(entry))) =>
        {
            call.entry = entry;
            OK
        }
        Progress::Done(Err(error)) => error,
        Progress::Done(_) => BAD_ABI
    }
}
