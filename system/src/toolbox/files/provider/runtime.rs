//! Park/replay boundary for owned filesystem operations. No app pointer, Files
//! guard or native I/O stack handoff survives a provider callback or wait.
#![forbid(unsafe_code)]
use alloc::{boxed::Box, string::String, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::{files::*, *};
use mac_filesystem_interface::{self as fs, Arguments, Operation};
use mac_module::ClassId;
use mac_object_table::shared::InvocationGate as _;
use spinning_top::Spinlock;

use crate::{drivers::{OwnedJob,
                      jobs::{Failure, subjects::Subject as IoSubject}},
            toolbox::handles::InvocationGate};
fn io_subject(owner: super::super::subject::Access<'_>) -> Result<IoSubject, Status>
{
    crate::drivers::jobs::subjects::resident()?.admit_current(&owner.authority()?)
}
mod forks;
mod paths;
mod mounts;
mod startup;
pub(crate) use startup::mount_startup_exports;
mod resources;
mod application;
mod metadata;
mod streams;
mod groups;
mod background;
mod resident;
pub(crate) use resident::{Reply as ResidentReply, Request as ResidentRequest};
pub(in crate::toolbox::files) mod transfer;
pub(crate) use application::{pending as application_pending, request as request_application};
pub(crate) use groups::request as request_group;
pub(crate) use metadata::{pending as metadata_pending, request as request_metadata};
pub(crate) use streams::request as request_stream;
mod writes;
pub(in crate::toolbox::files::provider) use forks::request_fork_cleanup;
pub(crate) use resources::request as request_resource;
pub(in crate::toolbox::files) use writes::request as request_write;
pub(in crate::toolbox::files::provider) use writes::request_write_cleanup;

fn reserve_flight_storage(flights: &mut Vec<Flight>) -> Result<(), Status>
{
    let bound = crate::toolbox::handles::Kind::DriverJobs.capacity();
    InvocationGate::with_system_allocation(|| {
        flights.try_reserve_exact(bound.saturating_sub(flights.len()))
    }).map_err(|_| NO_MEMORY)
}
static FLIGHTS: Spinlock<Vec<Flight>> = Spinlock::new(Vec::new());
static REAP: AtomicBool = AtomicBool::new(false);
static FORK_CLEANUP: AtomicBool = AtomicBool::new(false);
static WRITE_CLEANUP: AtomicBool = AtomicBool::new(false);
pub(crate) fn power_ready() -> bool
{
    FLIGHTS.lock().is_empty() && !resident::cleanup_pending()
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Record
{
    File,
    Scan,
    Write,
    Resource,
    Application,
    Metadata,
    Stream,
    Group
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Key
{
    record:    Record,
    operation: u32,
    volume:    u32,
    index:     u32,
    cnid:      u32,
    handle:    u64,
    fork:      u32,
    capacity:  usize,
    cursor:    DirectoryScanCursor,
    resource:  Option<(ResourceKey, usize)>,
    when:      u64,
    metadata:  Option<[u8; 64]>
}
enum Step
{
    Count,
    Mount(u32),
    Unmount(u32),
    Source(mounts::Acquire),
    Release
    {
        volume:  Option<u32>,
        closing: crate::drivers::file_sources::runtime::Closing,
        status:  Status
    },
    File(forks::Action),
    Write(writes::State),
    Resource(Box<resources::State>),
    Application(Box<application::State>),
    Metadata(metadata::State),
    Stream(streams::State),
    Group(groups::State),
    Scan(DirectoryScanCursor),
    PrepareName
    {
        volume: u32
    },
    Allocation
    {
        volume: u32,
        node:   u32
    },
    Lookup
    {
        volume: u32,
        node:   Option<u32>,
        parent: Option<u32>,
        at:     usize
    },
    Path
    {
        volume: u32,
        node:   u32
    },
    Catalog
    {
        volume: u32,
        node:   u32
    },
    Comment
    {
        volume: u32,
        node:   u32
    },
    Parent
    {
        volume: u32,
        node:   u32
    },
    Child
    {
        volume: u32,
        parent: u32,
        index:  u32
    },
    Cleanup
    {
        volume: u32,
        status: Status
    }
}
struct Flight
{
    thread:    u64,
    owner:     super::super::subject::Subject,
    key:       Key,
    provider:  u64,
    step:      Step,
    job:       OwnedJob,
    abandoned: bool,
    input:     String,
    path:      Option<super::path::Builder>
}
impl Flight
{
    fn abandon(&mut self)
    {
        self.abandoned = true;
        // Once provider teardown has acknowledged, backing Close is mandatory
        // cleanup. A dead waiter cannot cancel that close and recycle its mark.
        if !matches!(self.step,
                     Step::Release { .. } | Step::Source(mounts::Acquire::Cleanup { .. }))
           && !matches!(&self.step, Step::Application(state) if state.closing())
        {
            let _ = self.job.cancel();
        }
    }
}
// Keep the terminal metadata inline: allocation after publishing a mounted
// object would make delivery depend on another fallible heap reservation.
#[allow(clippy::large_enum_variant)]
enum Progress
{
    Done(Result<Reply, Status>),
    Pending(Flight)
}
#[allow(clippy::large_enum_variant)]
enum Reply
{
    Count(u32),
    Mounted(VolumeInfo),
    Entry(CatalogInfo),
    Opened(u32, u64),
    Scan(FileScanCall),
    Resource(Box<resources::Response>),
    Allocation(u64, u64),
    Bytes(Vec<u8>),
    Read
    {
        bytes:    Vec<u8>,
        position: u64,
        length:   u64
    },
    Unmounted,
    NoHandler,
    Group
    {
        handle:   u64,
        progress: fs::IconMoveProgress
    },
    Application(Box<super::super::application::Loaded>),
    Stream
    {
        handle:    u64,
        completed: bool,
        entry:     CatalogInfo,
        bytes:     Vec<u8>
    }
}

pub(crate) fn abandon_document_io(owner: u64, thread: u64, volume: u32, cnid: u32,
                                  application: bool)
                                  -> bool
{
    let mut flights = FLIGHTS.lock();
    let Some(flight) = flights.iter_mut().find(|flight| {
                                             flight.owner == owner
                                             && flight.thread == thread
                                             && flight.key.volume == volume
                                             && flight.key.cnid == cnid
                                             && if application
                                             {
                                                 flight.key.record == Record::Application
                                             }
                                             else
                                             {
                                                 flight.key.record == Record::File
                                                 && flight.key.operation == CATALOG
                                             }
                                         })
    else
    {
        return false;
    };
    flight.abandon();
    REAP.store(true, Ordering::Release);
    true
}
pub(crate) fn current_io_pending() -> bool
{
    let thread = crate::context::current_thread().0;
    FLIGHTS.lock().iter().any(|flight| flight.thread == thread && !flight.abandoned)
}
fn files<T>(call: impl FnOnce(&mut super::Manager) -> Result<T, Status>) -> Result<T, Status>
{
    crate::kernel::with_files(call).map_err(crate::state::AccessError::status)?
}
fn submit_new(owner: super::super::subject::Access<'_>, provider: u64, op: Operation,
              args: Arguments<'_>)
              -> Result<OwnedJob, Status>
{
    let subject = io_subject(owner)?;
    submit_args(&subject, provider, op, args)
}
enum IoObject
{
    Fork(u32),
    Write(u64),
    Stream(u64),
    Group(u64)
}
fn submit_object(object: IoObject, provider: u64, op: Operation, args: Arguments<'_>)
                 -> Result<OwnedJob, Status>
{
    let subject = files(|manager| {
        match object
        {
            IoObject::Fork(id) => manager.handles.io_subject(id),
            IoObject::Write(id) => manager.writes.io_subject(id),
            IoObject::Stream(id) => manager.streams.io_subject(id),
            IoObject::Group(id) => manager.groups.io_subject(id)
        }
    })?;
    submit_args(&subject, provider, op, args)
}
fn submit(subject: &IoSubject, provider: u64, op: Operation, volume: u32, source: u64)
          -> Result<OwnedJob, Status>
{
    submit_args(subject,
                provider,
                op,
                Arguments { volume,
                            auxiliary: source,
                            ..Default::default() })
}
pub(in crate::toolbox::files) fn submit_args(subject: &IoSubject, provider: u64, op: Operation,
                                             args: Arguments<'_>)
                                             -> Result<OwnedJob, Status>
{
    let allocation = subject.allocation().clone();
    crate::allocator::with_target(allocation, || {
        let mut bytes = Vec::new();
        let length = fs::HEADER_BYTES + args.payload.len();
        bytes.try_reserve_exact(length).map_err(|_| NO_MEMORY)?;
        bytes.resize(length, 0);
        args.encode(&mut bytes).map_err(|_| BAD_ARGUMENT)?;
        let size = match op
        {
            Operation::Exports => 4,
            Operation::PrepareName => fs::MAX_PREPARED_NAME_BYTES,
            Operation::Mount => fs::MAX_MOUNT_BYTES,
            Operation::Catalog
            | Operation::Lookup
            | Operation::BeginCreate
            | Operation::BeginReplace => fs::MAX_METADATA_BYTES,
            Operation::Scan => 4 + fs::MAX_METADATA_BYTES,
            Operation::FinishWrite => fs::MAX_WRITE_RECEIPT_BYTES,
            Operation::SetMetadata | Operation::Rename | Operation::Move | Operation::Remove =>
            {
                4 + fs::MAX_METADATA_BYTES
            }
            Operation::AbortWrite => fs::MAX_ABORT_RECEIPT_BYTES,
            Operation::Open => 16,
            Operation::CheckDirectory => 33,
            Operation::BeginIconMoves | Operation::StepIconMoves | Operation::AbortIconMoves =>
            {
                fs::ICON_MOVE_PROGRESS_BYTES
            }
            Operation::Read | Operation::ReadWrite =>
            {
                usize::try_from(args.length).map_err(|_| BAD_ARGUMENT)?
            }
            _ => 0
        };
        crate::drivers::submit_background_to(subject,
                                             provider,
                                             ClassId(fs::CLASS),
                                             fs::VERSION,
                                             op as u32,
                                             bytes,
                                             size)
    })
}
fn status(failure: Failure) -> Status
{
    match failure
    {
        Failure::Service(status) => status,
        Failure::Cancelled => OPERATION_CANCELLED
    }
}
fn cleanup_mount(mut flight: Flight, volume: u32, error: Status) -> Progress
{
    match submit(flight.job.subject(), flight.provider, Operation::Unmount, volume, 0)
    {
        Ok(job) =>
        {
            flight.job = job;
            flight.step = Step::Cleanup { volume,
                                          status: error };
            Progress::Pending(flight)
        }
        Err(_) =>
        {
            let _ = files(|manager| manager.quarantine_provider_mount(volume));
            Progress::Done(Err(ROLLBACK_FAILED))
        }
    }
}
fn retire_unstarted_export(volume: u32) -> Result<Reply, Status>
{
    let (_, source) = files(|manager| manager.provider_source(volume))?;
    if !matches!(source, super::Source::Export(_))
    {
        return Err(BAD_ARGUMENT);
    }
    files(|manager| manager.retire_provider_mount(volume)).map(|()| Reply::Unmounted)
}
fn retired(volume: Option<u32>, status: Status) -> Progress
{
    let result = volume.map_or(Ok(()), |v| files(|m| m.retire_provider_mount(v)));
    Progress::Done(result.and(if status == OK { Ok(Reply::Unmounted) } else { Err(status) }))
}
fn retire_mount(flight: Flight, volume: u32, status: Status) -> Progress
{
    match files(|manager| manager.provider_source(volume))
    {
        Ok((_, super::Source::Export(_))) => retired(Some(volume), status),
        Ok((_, super::Source::File(source))) =>
        {
            mounts::release(flight, source, Some(volume), status)
        }
        Err(error) => Progress::Done(Err(error))
    }
}
fn finish_read(volume: u32, result: Result<CatalogInfo, Status>) -> Progress
{
    let released = files(|manager| manager.end_provider_read(volume));
    Progress::Done(released.and(result).map(Reply::Entry))
}
fn continue_read(mut flight: Flight, operation: Operation, args: Arguments<'_>) -> Progress
{
    match submit_args(flight.job.subject(), flight.provider, operation, args)
    {
        Ok(job) =>
        {
            flight.job = job;
            Progress::Pending(flight)
        }
        Err(error) => finish_read(args.volume, Err(error))
    }
}
fn release_progress(flight: &mut Flight, reply: Option<Result<Vec<u8>, Status>>)
                    -> Option<Result<Reply, Status>>
{
    let Step::Release { volume,
                        closing,
                        status } = &mut flight.step
    else
    {
        return Some(Err(BAD_ABI));
    };
    match crate::drivers::file_sources::runtime::finish_release(closing, reply)
    {
        Ok(false) => None,
        Ok(true) => Some(volume.map_or(Ok(()), |v| files(|m| m.retire_provider_mount(v)))
                               .and(if *status == OK
                                    {
                                        Ok(Reply::Unmounted)
                                    }
                                    else
                                    {
                                        Err(*status)
                                    })),
        Err(error) =>
        {
            if let Some(v) = *volume
            {
                let _ = files(|m| m.quarantine_provider_mount(v));
            }
            Some(Err(error))
        }
    }
}
fn advance(mut flight: Flight) -> Progress
{
    let allocation = flight.job.subject().allocation().clone();
    crate::allocator::with_target(allocation, || {
        if matches!(&flight.step, Step::Release { closing, .. } if closing.has_reply())
        {
            return match release_progress(&mut flight, None)
            {
                Some(result) => Progress::Done(result),
                None => Progress::Pending(flight)
            };
        }
        let (result, never_sent) = match flight.job.poll()
        {
            Ok(None) => return Progress::Pending(flight),
            Ok(Some(done)) =>
            {
                let never_sent = matches!(done.result, Err(Failure::Cancelled));
                (done.result.map_err(status), never_sent)
            }
            Err(error) => (Err(error), false)
        };
        match flight.step
        {
            Step::File(_) => forks::advance(flight, result, never_sent),
            Step::Source(_) => mounts::advance(flight, result, never_sent),
            Step::Release { .. } =>
            {
                match release_progress(&mut flight, Some(result))
                {
                    Some(result) => Progress::Done(result),
                    None => Progress::Pending(flight)
                }
            }
            Step::Write(_) => writes::advance(flight, result, never_sent),
            Step::Resource(_) => resources::advance(flight, result),
            Step::Application(_) => application::advance(flight, result, never_sent),
            Step::Metadata(_) => metadata::advance(flight, result, never_sent),
            Step::Stream(_) => streams::advance(flight, result, never_sent),
            Step::Group(_) => groups::advance(flight, result, never_sent),
            Step::Scan(cursor) =>
            {
                let result = result.and_then(|bytes| super::scan::reply(cursor, &bytes));
                Progress::Done(files(|manager| manager.end_provider_read(cursor.volume)).and(result).map(Reply::Scan))
            }
            Step::PrepareName { volume } =>
            {
                Progress::Done(files(|manager| manager.end_provider_read(volume)).and(result)
                                                                                 .map(Reply::Bytes))
            }
            Step::Allocation { volume,
                               node } =>
            {
                let result =
                    result.and_then(|bytes| super::catalog(volume, &bytes, Some(node), None))
                          .map(|entry| {
                              if entry.is_directory()
                              {
                                  Reply::Allocation(0, 0)
                              }
                              else
                              {
                                  Reply::Allocation(ALLOCATION_UNKNOWN, ALLOCATION_UNKNOWN)
                              }
                          });
                Progress::Done(files(|manager| manager.end_provider_read(volume)).and(result))
            }
            Step::Lookup { .. } | Step::Path { .. } => paths::advance(flight, result),
            Step::Comment { volume,
                            node } =>
            {
                // Comments are already part of the provider's catalog record.
                // Do not enumerate children merely to read a directory comment.
                let result = result.and_then(|bytes| {
                                       let metadata =
                                           fs::Metadata::decode(&bytes).map_err(|_| BAD_ABI)?;
                                       super::catalog_metadata(volume, metadata, Some(node), None)?;
                                       let mut text = Vec::new();
                                       text.try_reserve_exact(metadata.comment.len())
                                           .map_err(|_| NO_MEMORY)?;
                                       text.extend_from_slice(metadata.comment.as_bytes());
                                       Ok(Reply::Bytes(text))
                                   });
                Progress::Done(files(|manager| manager.end_provider_read(volume)).and(result))
            }
            Step::Catalog { volume,
                            node } =>
            {
                let result =
                    result.and_then(|bytes| super::catalog(volume, &bytes, Some(node), None))
                          .and_then(|entry| {
                              if entry.is_directory()
                                 && entry.child_count == fs::CHILD_COUNT_UNKNOWN
                              {
                                  Err(BAD_ABI)
                              }
                              else
                              {
                                  Ok(entry)
                              }
                          });
                finish_read(volume, result)
            }
            Step::Parent { volume,
                           node } =>
            {
                let result =
                    result.and_then(|bytes| super::catalog(volume, &bytes, Some(node), None));
                match result
                {
                    Ok(entry) if !flight.abandoned =>
                    {
                        let parent = if entry.id == ROOT_ID { ROOT_ID } else { entry.parent };
                        if parent == entry.id
                        {
                            if entry.child_count == fs::CHILD_COUNT_UNKNOWN
                            {
                                return finish_read(volume, Err(BAD_ABI));
                            }
                            return finish_read(volume, Ok(entry));
                        }
                        flight.step = Step::Catalog { volume,
                                                      node: parent };
                        continue_read(flight,
                                      Operation::Catalog,
                                      Arguments { volume,
                                                  node: parent,
                                                  flags: fs::INCLUDE_CHILD_COUNT,
                                                  ..Default::default() })
                    }
                    other => finish_read(volume, other)
                }
            }
            Step::Child { volume,
                          parent,
                          index } =>
            {
                match result.and_then(|bytes| super::scan_entry(volume, parent, &bytes))
                {
                    Ok(None) if !flight.abandoned =>
                    {
                        continue_read(flight,
                                      Operation::Scan,
                                      Arguments { volume,
                                                  parent,
                                                  offset: u64::from(index),
                                                  ..Default::default() })
                    }
                    Ok(None) => finish_read(volume, Err(OPERATION_CANCELLED)),
                    Ok(Some(entry)) => finish_read(volume, Ok(entry)),
                    Err(error) => finish_read(volume, Err(error))
                }
            }
            Step::Count =>
            {
                let result = result.and_then(|bytes| {
                                       let bytes =
                                           bytes.as_slice().try_into().map_err(|_| BAD_ABI)?;
                                       Ok(Reply::Count(u32::from_le_bytes(bytes)))
                                   });
                Progress::Done(result)
            }
            Step::Mount(volume) =>
            {
                if never_sent
                {
                    // The queue guarantees this cancellation never reached the
                    // provider. Retiring its reservation needs no redundant RPC.
                    return retire_mount(flight, volume, OPERATION_CANCELLED);
                }
                let result = if flight.abandoned
                {
                    Err(OPERATION_CANCELLED)
                }
                else
                {
                    result.and_then(|bytes| {
                              files(|manager| manager.publish_provider_mount(volume, &bytes))
                          })
                };
                match result
                {
                    Ok(info) => Progress::Done(Ok(Reply::Mounted(info))),
                    Err(error) => cleanup_mount(flight, volume, error)
                }
            }
            Step::Unmount(volume) =>
            {
                match result
                {
                    Ok(bytes) if bytes.is_empty() => retire_mount(flight, volume, OK),
                    Err(NOT_FOUND) => retire_mount(flight, volume, OK),
                    error =>
                    {
                        let _ = files(|manager| manager.quarantine_provider_mount(volume));
                        Progress::Done(Err(error.err().unwrap_or(BAD_ABI)))
                    }
                }
            }
            Step::Cleanup { volume,
                            status } =>
            {
                if matches!(&result, Ok(bytes) if bytes.is_empty())
                   || matches!(result, Err(NOT_FOUND))
                {
                    retire_mount(flight, volume, status)
                }
                else
                {
                    let _ = files(|manager| manager.quarantine_provider_mount(volume));
                    Progress::Done(Err(ROLLBACK_FAILED))
                }
            }
        }
    })
}

fn reply(call: &mut FileCall, output: &mut [u8], result: Result<Reply, Status>) -> Status
{
    match result
    {
        Ok(Reply::Allocation(data, resource)) =>
        {
            call.position = data;
            call.length = resource;
            OK
        }
        Ok(Reply::Scan(_)
           | Reply::Resource(_)
           | Reply::Application(_)
           | Reply::Stream { .. }
           | Reply::Group { .. }) => BAD_ABI,
        Ok(Reply::Bytes(bytes)) =>
        {
            call.length = bytes.len() as u64;
            if output.len() < bytes.len()
            {
                return NO_MEMORY;
            }
            output[..bytes.len()].copy_from_slice(&bytes);
            call.transferred = bytes.len();
            OK
        }
        Ok(Reply::Opened(handle, length)) =>
        {
            call.handle = handle;
            call.length = length;
            call.position = 0;
            OK
        }
        Ok(Reply::Read { bytes,
                         position,
                         length }) =>
        {
            if bytes.len() > output.len()
            {
                return BAD_ABI;
            }
            output[..bytes.len()].copy_from_slice(&bytes);
            call.transferred = bytes.len();
            call.position = position;
            call.length = length;
            OK
        }
        Ok(Reply::Count(count)) =>
        {
            call.index = count;
            OK
        }
        Ok(Reply::Mounted(info)) =>
        {
            if call.operation == OPEN_ITEM
            {
                call.index = 1;
            }
            call.volume = info.id;
            call.volume_info = info;
            OK
        }
        Ok(Reply::Unmounted) => OK,
        Ok(Reply::NoHandler) =>
        {
            if call.operation == OPEN_ITEM
            {
                call.index = 0;
                OK
            }
            else
            {
                NOT_SUPPORTED
            }
        }
        Ok(Reply::Entry(entry)) =>
        {
            call.entry = entry;
            OK
        }
        Err(status) => status
    }
}
fn waiting(flight: Flight) -> Status
{
    // The resident retains the result; only a generation-checked scheduler wake
    // is attached to this caller. The outer trap drops all borrows before park.
    let result = flight.job.defer_current();
    let mut flights = FLIGHTS.lock();
    let mut flight = flight;
    if result.is_err()
    {
        flight.abandon();
        REAP.store(true, Ordering::Release);
    }
    flights.push(flight); // capacity was reserved before any mount side effect
    result.err().unwrap_or(SERVICE_BUSY)
}
fn claim(owner: u64, thread: u64, key: Key, name: &str) -> Result<Option<Flight>, Status>
{
    let mut flights = FLIGHTS.lock();
    if let Some(at) = flights.iter().position(|flight| flight.thread == thread)
    {
        if flights[at].key != key
           || flights[at].owner != owner
           || flights[at].abandoned
           || flights[at].input != name
        {
            flights[at].abandon();
            REAP.store(true, Ordering::Release);
            return Err(BAD_ABI);
        }
        return Ok(Some(flights.swap_remove(at)));
    }
    reserve_flight_storage(&mut flights)?;
    Ok(None)
}
pub(crate) fn request_scan(owner: u64, call: &mut FileScanCall) -> Status
{
    let thread = crate::context::current_thread().0;
    if thread <= 1
    {
        return WRONG_THREAD;
    }
    let key = Key { record:    Record::Scan,
                    operation: SCAN_DIRECTORY,
                    volume:    call.volume,
                    cnid:      call.directory,
                    index:     0,
                    handle:    0,
                    fork:      0,
                    capacity:  0,
                    cursor:    call.cursor,
                    resource:  None,
                    metadata:  None,
                    when:      0 };
    match claim(owner, thread, key, "")
    {
        Ok(Some(flight)) =>
        {
            return match advance(flight)
            {
                Progress::Done(Ok(Reply::Scan(reply))) =>
                {
                    *call = reply;
                    OK
                }
                Progress::Done(Err(error)) => error,
                Progress::Done(_) => BAD_ABI,
                Progress::Pending(flight) => waiting(flight)
            };
        }
        Ok(None) => (),
        Err(error) => return error
    }
    let (provider, cursor) = match files(|manager| manager.begin_provider_scan(call))
    {
        Ok(prepared) => prepared,
        Err(error) => return error
    };
    let job = match submit_new(owner.into(),
                               provider,
                               Operation::Scan,
                               Arguments { volume: cursor.volume,
                                           parent: cursor.directory,
                                           offset: u64::from(cursor.index),
                                           ..Default::default() })
    {
        Ok(job) => job,
        Err(error) =>
        {
            let _ = files(|manager| manager.end_provider_read(cursor.volume));
            return error;
        }
    };
    waiting(Flight { thread,
                     owner: owner.into(),
                     key,
                     provider,
                     step: Step::Scan(cursor),
                     job,
                     abandoned: false,
                     input: String::new(),
                     path: None })
}

pub(crate) fn request(owner: u64, call: &mut FileCall, name: &str, output: &mut [u8]) -> Status
{
    // Replay identity includes resident-derived paths as well as app strings.
    let name = if call.operation == TRASH_DIRECTORY { "Trash" } else { name };
    let thread = crate::context::current_thread().0;
    if thread <= 1
    {
        return WRONG_THREAD;
    }
    let key = Key { record:    Record::File,
                    operation: call.operation,
                    volume:    call.volume,
                    index:     call.index,
                    cnid:      call.cnid,
                    handle:    u64::from(call.handle),
                    fork:      call.fork,
                    capacity:  output.len(),
                    cursor:    DirectoryScanCursor::default(),
                    resource:  None,
                    metadata:  None,
                    when:      if matches!(call.operation, NEW_FOLDER | TRASH_DIRECTORY)
                    {
                        call.position
                    }
                    else
                    {
                        0
                    } };
    match claim(owner, thread, key, name)
    {
        Ok(Some(flight)) =>
        {
            return match advance(flight)
            {
                Progress::Done(result) => reply(call, output, result),
                Progress::Pending(flight) => waiting(flight)
            };
        }
        Ok(None) => (),
        Err(error) => return error
    }
    if matches!(call.operation, LOOKUP | PATH)
    {
        return paths::start(owner, thread, key, call, name);
    }
    if call.operation == NEW_FOLDER
    {
        return writes::folder(owner, thread, key, call, name);
    }
    if call.operation == TRASH_DIRECTORY
    {
        return paths::start(owner, thread, key, call, name);
    }
    if matches!(call.operation, OPEN | READ | CLOSE)
    {
        return forks::start(owner, thread, key, call, output);
    }
    if matches!(call.operation, MOUNT_FILE | OPEN_ITEM)
    {
        return mounts::start_file(owner, thread, key, call, output);
    }
    if matches!(call.operation, CATALOG | PARENT | DIRECTORY_ENTRY | ALLOCATION_INFO | COMMENT)
    {
        let provider = match files(|manager| manager.begin_provider_read(call.volume))
        {
            Ok(provider) => provider,
            Err(error) => return error
        };
        let volume = call.volume;
        let node = if call.cnid == 0 { ROOT_ID } else { call.cnid };
        let (step, operation, args) = if call.operation == DIRECTORY_ENTRY
        {
            (Step::Child { volume,
                           parent: node,
                           index: call.index },
             Operation::Scan,
             Arguments { volume,
                         parent: node,
                         offset: u64::from(call.index),
                         ..Default::default() })
        }
        else
        {
            (if call.operation == PARENT
             {
                 Step::Parent { volume,
                                node }
             }
             else if call.operation == COMMENT
             {
                 Step::Comment { volume,
                                 node }
             }
             else
             {
                 if call.operation == ALLOCATION_INFO
                 {
                     Step::Allocation { volume,
                                        node }
                 }
                 else
                 {
                     Step::Catalog { volume,
                                     node }
                 }
             },
             Operation::Catalog,
             Arguments { volume,
                         node,
                         flags: if call.operation == CATALOG
                                   || call.operation == PARENT && node == ROOT_ID
                         {
                             fs::INCLUDE_CHILD_COUNT
                         }
                         else
                         {
                             0
                         },
                         ..Default::default() })
        };
        let job = match submit_new(owner.into(), provider, operation, args)
        {
            Ok(job) => job,
            Err(error) =>
            {
                let _ = files(|manager| manager.end_provider_read(volume));
                return error;
            }
        };
        return waiting(Flight { thread,
                                owner: owner.into(),
                                key,
                                provider,
                                step,
                                job,
                                abandoned: false,
                                input: String::new(),
                                path: None });
    }
    let provider = if call.operation == UNMOUNT_VOLUME
    {
        match files(|manager| manager.begin_provider_unmount(call.volume))
        {
            Ok(id) => id,
            Err(error) => return error
        }
    }
    else
    {
        match crate::drivers::find_kernel_provider(ClassId(fs::CLASS),
                                                   fs::VERSION,
                                                   fs::SOURCE_HOST_EXPORT)
        {
            Ok(id) => id,
            Err(NOT_FOUND) if call.operation == HOST_EXPORT_COUNT =>
            {
                return reply(call, output, Ok(Reply::Count(0)));
            }
            Err(error) => return error
        }
    };
    let (step, operation, volume, source) = match call.operation
    {
        HOST_EXPORT_COUNT => (Step::Count, Operation::Exports, 0, 0),
        MOUNT_HOST_EXPORT =>
        {
            return match startup::host_mount(owner.into(), provider, call.index)
            {
                Ok(startup::HostMount::Existing(info)) =>
                {
                    reply(call, output, Ok(Reply::Mounted(info)))
                }
                Ok(startup::HostMount::Pending(volume, job)) =>
                {
                    waiting(Flight { thread,
                                     owner: owner.into(),
                                     key,
                                     provider,
                                     step: Step::Mount(volume),
                                     job,
                                     abandoned: false,
                                     input: String::new(),
                                     path: None })
                }
                Err(error) => error
            };
        }
        UNMOUNT_VOLUME => (Step::Unmount(call.volume), Operation::Unmount, call.volume, 0),
        _ => return BAD_ARGUMENT
    };
    let job = match io_subject(owner.into()).and_then(|subject| {
                                                submit(&subject, provider, operation, volume,
                                                       source)
                                            })
    {
        Ok(job) => job,
        Err(error) =>
        {
            if operation == Operation::Unmount
            {
                let _ = files(|manager| manager.quarantine_provider_mount(volume));
            }
            return error;
        }
    };
    waiting(Flight { thread,
                     owner: owner.into(),
                     key,
                     provider,
                     step,
                     job,
                     abandoned: false,
                     input: String::new(),
                     path: None })
}

/// Thread exit only requests cleanup. The resident record retains buffers,
/// provider IDs and actual completion until cancellation has been acknowledged.
pub(crate) fn abandon_thread(thread: u64)
{
    let mut flights = FLIGHTS.lock();
    if let Some(flight) = flights.iter_mut().find(|flight| flight.thread == thread)
    {
        flight.abandon();
        REAP.store(true, Ordering::Release);
    }
}
pub(crate) fn pump()
{
    resident::pump_cleanup();
    crate::drivers::file_sources::runtime::pump();
    transfer::pump();
    if super::super::groups::needs_cleanup()
    {
        groups::pump_cleanup();
    }
    if super::super::streams::needs_cleanup()
    {
        streams::pump_cleanup();
        let _ = files(|manager| {
            manager.pump_stream_cleanup();
            Ok(())
        });
    }
    super::super::watch::pump();
    forks::pump_cleanup();
    writes::pump_cleanup();
    if !REAP.load(Ordering::Acquire)
    {
        return;
    }
    let mut flights = FLIGHTS.lock();
    let Some(at) = flights.iter().position(|f| f.abandoned)
    else
    {
        REAP.store(false, Ordering::Release);
        return;
    };
    let flight = flights.swap_remove(at);
    drop(flights);
    // No manager, queue or operation-list guard survives completion processing.
    let document = flight.key.record == Record::Application
                   || flight.key.record == Record::File && flight.key.operation == CATALOG;
    match advance(flight)
    {
        Progress::Pending(flight) => FLIGHTS.lock().push(flight),
        Progress::Done(_) =>
        {
            if document
            {
                crate::toolbox::processes::documents::io_changed();
            }
        }
    }
}
