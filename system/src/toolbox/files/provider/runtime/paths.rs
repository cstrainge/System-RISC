//! Owned path traversal. Intermediate directories need metadata, not a complete
//! enumeration just to obtain a child count that the traversal will discard.
#![forbid(unsafe_code)]
use super::*;
fn volume(step: &Step) -> Result<u32, Status>
{
    match step
    {
        Step::Lookup { volume,
                       .. }
        | Step::Path { volume,
                     .. } => Ok(*volume),
        _ => Err(BAD_ABI)
    }
}
fn pending(mut flight: Flight, job: Result<OwnedJob, Status>) -> Progress
{
    match job
    {
        Ok(job) =>
        {
            flight.job = job;
            Progress::Pending(flight)
        }
        Err(error) =>
        {
            match volume(&flight.step)
            {
                Ok(volume) => finish_read(volume, Err(error)),
                Err(error) => Progress::Done(Err(error))
            }
        }
    }
}
fn finish_path(volume: u32, result: Result<Vec<u8>, Status>) -> Progress
{
    Progress::Done(files(|manager| manager.end_provider_read(volume)).and(result).map(Reply::Path))
}
pub(super) fn advance(mut flight: Flight, result: Result<Vec<u8>, Status>) -> Progress
{
    let volume = match volume(&flight.step)
    {
        Ok(volume) => volume,
        Err(error) => return Progress::Done(Err(error))
    };
    if flight.abandoned
    {
        return finish_read(volume, Err(OPERATION_CANCELLED));
    }
    match flight.step
    {
        Step::Lookup { node,
                       parent,
                       mut at,
                       .. } =>
        {
            let entry =
                match result.and_then(|bytes| super::super::catalog(volume, &bytes, node, parent))
                {
                    Ok(entry) => entry,
                    Err(error) => return finish_read(volume, Err(error))
                };
            while let Some((next, component)) = super::super::path::component(&flight.input, at)
            {
                at = next;
                if entry.kind != DIRECTORY
                {
                    return finish_read(volume, Err(BAD_ARGUMENT));
                }
                if component == "." || component == ".." && entry.id == ROOT_ID
                {
                    continue;
                }
                let (operation, args, node, parent) = if component == ".."
                {
                    (Operation::Catalog,
                     Arguments { volume,
                                 node: entry.parent,
                                 ..Default::default() },
                     Some(entry.parent),
                     None)
                }
                else
                {
                    let args = Arguments { volume,
                                           parent: entry.id,
                                           payload: component.as_bytes(),
                                           ..Default::default() };
                    if args.name().is_err()
                    {
                        return finish_read(volume, Err(BAD_ARGUMENT));
                    }
                    (Operation::Lookup, args, None, Some(entry.id))
                };
                let job = submit_args(flight.provider, operation, args);
                flight.step = Step::Lookup { volume,
                                             node,
                                             parent,
                                             at };
                return pending(flight, job);
            }
            if flight.input.ends_with('/') && entry.kind != DIRECTORY
            {
                return finish_read(volume, Err(BAD_ARGUMENT));
            }
            if entry.kind == DIRECTORY
            {
                // Obtain/validate the final count only; ancestors were metadata
                // queries and never enumerated their own children just for it.
                let job = submit_args(flight.provider,
                                      Operation::Catalog,
                                      Arguments { volume,
                                                  node: entry.id,
                                                  flags: fs::INCLUDE_CHILD_COUNT,
                                                  ..Default::default() });
                flight.step = Step::Catalog { volume,
                                              node: entry.id };
                return match job
                {
                    Ok(job) =>
                    {
                        flight.job = job;
                        Progress::Pending(flight)
                    }
                    Err(error) => finish_read(volume, Err(error))
                };
            }
            finish_read(volume, Ok(entry))
        }
        Step::Path { node,
                     .. } =>
        {
            let entry = match result.and_then(|bytes| {
                                        super::super::catalog(volume, &bytes, Some(node), None)
                                    })
            {
                Ok(entry) => entry,
                Err(error) => return finish_path(volume, Err(error))
            };
            let Some(path) = &mut flight.path
            else
            {
                return finish_path(volume, Err(BAD_ABI));
            };
            if entry.id == ROOT_ID
            {
                let path = flight.path.take().ok_or(BAD_ABI).and_then(|path| path.finish());
                return finish_path(volume, path);
            }
            if let Err(error) = path.push(&entry)
            {
                return finish_path(volume, Err(error));
            }
            let job = submit_args(flight.provider,
                                  Operation::Catalog,
                                  Arguments { volume,
                                              node: entry.parent,
                                              ..Default::default() });
            flight.step = Step::Path { volume,
                                       node: entry.parent };
            pending(flight, job)
        }
        _ => finish_read(volume, Err(BAD_ABI))
    }
}
pub(super) fn start(owner: u64, thread: u64, key: Key, call: &FileCall, name: &str) -> Status
{
    if name.len() > super::super::super::MAX_PATH || name.contains('\0')
    {
        return BAD_ARGUMENT;
    }
    let mut input = String::new();
    if input.try_reserve_exact(name.len()).is_err()
    {
        return NO_MEMORY;
    }
    input.push_str(name);
    let volume = call.volume;
    let provider = match files(|manager| manager.begin_provider_read(volume))
    {
        Ok(provider) => provider,
        Err(error) => return error
    };
    let node = if call.cnid == 0 || call.operation == LOOKUP && name.starts_with('/')
    {
        ROOT_ID
    }
    else
    {
        call.cnid
    };
    let (step, path) = if call.operation == LOOKUP
    {
        (Step::Lookup { volume,
                        node: Some(node),
                        parent: None,
                        at: 0 },
         None)
    }
    else
    {
        (Step::Path { volume,
                      node },
         Some(super::super::path::Builder::default()))
    };
    let job = match submit_args(provider,
                                Operation::Catalog,
                                Arguments { volume,
                                            node,
                                            ..Default::default() })
    {
        Ok(job) => job,
        Err(error) =>
        {
            let _ = files(|manager| manager.end_provider_read(volume));
            return error;
        }
    };
    waiting(Flight { thread,
                     owner,
                     key,
                     provider,
                     step,
                     job,
                     abandoned: false,
                     input,
                     path })
}
