//! Composite application load, parked/replayed on owned provider jobs. A private
//! generation-tagged fork mark stays reserved until Close is acknowledged.
#![forbid(unsafe_code)]
use mac_formats::resource::loader::Loader;

use super::{super::super::application::Loaded, *};

enum Stage
{
    Catalog,
    Open,
    Read,
    Close
}
pub(super) struct State
{
    selection: super::super::super::application::Selection,
    stage:     Stage,
    volume:    u32,
    handle:    u32,
    catalog:   Option<CatalogInfo>,
    loader:    Option<Loader>,
    reading:   bool,
    outcome:   Option<Result<Box<Loaded>, Status>>
}
impl State
{
    pub(super) fn closing(&self) -> bool
    {
        matches!(self.stage, Stage::Close)
    }
}
pub(crate) fn pending() -> bool
{
    let thread = crate::context::current_thread().0;
    FLIGHTS.lock().iter().any(|f| f.thread == thread && f.key.record == Record::Application)
}
fn retire(handle: u32) -> Result<(), Status>
{
    files(|m| m.retire_provider_file(handle))
}
fn quarantine(handle: u32)
{
    let _ = files(|m| m.quarantine_provider_file(handle));
}
fn close(mut flight: Flight, mut state: Box<State>, outcome: Result<Box<Loaded>, Status>)
         -> Progress
{
    if state.reading
    {
        if files(|m| m.end_resource_inspection(flight.owner.label(), state.handle, 0, None)).is_err()
        {
            quarantine(state.handle);
            return Progress::Done(Err(ROLLBACK_FAILED));
        }
        state.reading = false;
    }
    let result = files(|m| m.cleanup_provider_file(state.handle)).and_then(|volume| {
                     submit_args(flight.job.subject(),
                                 flight.provider,
                                 Operation::Close,
                                 Arguments { volume,
                                             token: u64::from(state.handle),
                                             ..Default::default() })
                 });
    match result
    {
        Ok(job) =>
        {
            state.loader = None;
            state.outcome = Some(outcome);
            state.stage = Stage::Close;
            flight.step = Step::Application(state);
            flight.job = job;
            Progress::Pending(flight)
        }
        Err(_) =>
        {
            quarantine(state.handle);
            Progress::Done(Err(ROLLBACK_FAILED))
        }
    }
}
fn read(provider: u64, state: &State) -> Result<OwnedJob, Status>
{
    let (offset, count) = state.loader.as_ref().and_then(Loader::request).ok_or(BAD_ABI)?;
    submit_object(IoObject::Fork(state.handle),
                  provider,
                  Operation::Read,
                  Arguments { volume: state.volume,
                              token: u64::from(state.handle),
                              flags: RESOURCE_FORK,
                              offset,
                              length: count as u64,
                              ..Default::default() })
}
pub(super) fn advance(mut flight: Flight, result: Result<Vec<u8>, Status>, never_sent: bool)
                      -> Progress
{
    let Step::Application(mut state) = core::mem::replace(&mut flight.step, Step::Count)
    else
    {
        return Progress::Done(Err(BAD_ABI));
    };
    if state.closing()
    {
        if matches!(&result, Ok(bytes) if bytes.is_empty()) || matches!(result, Err(NOT_FOUND))
        {
            return Progress::Done(retire(state.handle).and_then(|()| {
                                                          if flight.abandoned
                                                          {
                                                              Err(OPERATION_CANCELLED)
                                                          }
                                                          else
                                                          {
                                                              state.outcome
                                                                   .take()
                                                                   .ok_or(BAD_ABI)?
                                                                   .map(Reply::Application)
                                                          }
                                                      }));
        }
        quarantine(state.handle);
        return Progress::Done(Err(ROLLBACK_FAILED));
    }
    // Catalog cannot have opened a provider token. A cancelled, never-dispatched
    // Open also needs only local retirement; every other Open outcome is closed.
    let no_token =
        matches!(state.stage, Stage::Catalog) || matches!(state.stage, Stage::Open) && never_sent;
    let result = if flight.abandoned { Err(OPERATION_CANCELLED) } else { result };
    let next = (|| {
        let bytes = result?;
        match state.stage
        {
            Stage::Catalog =>
            {
                let catalog =
                    super::super::catalog(state.volume, &bytes, Some(flight.key.cnid), None)?;
                if catalog.kind != FILE || catalog.file_type != *b"APPL"
                {
                    return Err(BAD_ARGUMENT);
                }
                state.catalog = Some(catalog);
                let job = submit_args(flight.job.subject(),
                                      flight.provider,
                                      Operation::Open,
                                      Arguments { volume: state.volume,
                                                  node: catalog.id,
                                                  token: u64::from(state.handle),
                                                  ..Default::default() })?;
                state.stage = Stage::Open;
                Ok(Some(job))
            }
            Stage::Open =>
            {
                let length = files(|m| m.publish_provider_file(state.handle, &bytes))?;
                let _ = files(|m| m.begin_resource_inspection(flight.owner.label(), state.handle))?;
                state.reading = true;
                state.loader =
                    Some(Loader::selected(length,state.selection.budget(),state.selection.includes()).map_err(resources_format_status)?);
                state.stage = Stage::Read;
                read(flight.provider, &state).map(Some)
            }
            Stage::Read =>
            {
                let loader = state.loader.as_mut().ok_or(BAD_ABI)?;
                loader.received(&bytes).map_err(resources_format_status)?;
                if loader.request().is_some()
                {
                    read(flight.provider, &state).map(Some)
                }
                else
                {
                    Ok(None)
                }
            }
            Stage::Close => Err(BAD_ABI)
        }
    })();
    match next
    {
        Ok(Some(job)) =>
        {
            flight.job = job;
            flight.step = Step::Application(state);
            Progress::Pending(flight)
        }
        Ok(None) =>
        {
            let outcome = (|| {
                let resources = state.loader
                                     .take()
                                     .ok_or(BAD_ABI)?
                                     .finish()
                                     .map_err(resources_format_status)?;
                Box::try_new(Loaded { catalog: state.catalog.ok_or(BAD_ABI)?,
                                      resources }).map_err(|_| NO_MEMORY)
            })();
            close(flight, state, outcome)
        }
        Err(error) if no_token => Progress::Done(retire(state.handle).and(Err(error))),
        Err(error) => close(flight, state, Err(error))
    }
}
fn resources_format_status(error: mac_formats::Error) -> Status
{
    super::super::super::resource::application_format_status(error)
}
fn deliver(progress: Progress) -> Result<Box<Loaded>, Status>
{
    match progress
    {
        Progress::Pending(flight) => Err(waiting(flight)),
        Progress::Done(Ok(Reply::Application(loaded))) => Ok(loaded),
        Progress::Done(Err(error)) => Err(error),
        Progress::Done(_) => Err(BAD_ABI)
    }
}
pub(crate) fn request(owner: u64, volume: u32, cnid: u32,
                      selection: super::super::super::application::Selection)
                      -> Result<Box<Loaded>, Status>
{
    let thread = crate::context::current_thread().0;
    if thread <= 1
    {
        return Err(WRONG_THREAD);
    }
    let key = Key { record: Record::Application,
                    operation: if selection == super::super::super::application::Selection::All
                    {
                        mac_abi::processes::LAUNCH
                    }
                    else
                    {
                        u32::MAX
                    },
                    volume,
                    cnid,
                    index: 0,
                    handle: 0,
                    fork: RESOURCE_FORK,
                    capacity: 0,
                    cursor: DirectoryScanCursor::default(),
                    resource: None,
                    metadata: None,
                    when: 0 };
    if let Some(flight) = claim(owner, thread, key, "")?
    {
        return deliver(super::advance(flight));
    }
    // Allocate all state before reserving the fork mark or submitting any work.
    let mut state = Box::try_new(State { selection,
                                         stage: Stage::Catalog,
                                         volume,
                                         handle: 0,
                                         catalog: None,
                                         loader: None,
                                         reading: false,
                                         outcome: None }).map_err(|_| NO_MEMORY)?;
    let authority = crate::toolbox::files::creation_authority(owner)?;
    let (provider, handle) =
        files(|m| m.reserve_provider_file_for(owner, &authority, volume, cnid, RESOURCE_FORK))?;
    state.handle = handle;
    let job = match submit_object(IoObject::Fork(handle),
                                  provider,
                                  Operation::Catalog,
                                  Arguments { volume,
                                              node: cnid,
                                              ..Default::default() })
    {
        Ok(job) => job,
        Err(error) =>
        {
            retire(handle)?;
            return Err(error);
        }
    };
    deliver(Progress::Pending(Flight { thread,
                                       owner: owner.into(),
                                       key,
                                       provider,
                                       step: Step::Application(state),
                                       job,
                                       abandoned: false,
                                       input: String::new(),
                                       path: None }))
}
