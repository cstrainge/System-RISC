//! Park/replay boundary for owned filesystem operations. No app pointer, Files
//! guard or native I/O stack handoff survives a provider callback or wait.
#![forbid(unsafe_code)]
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::{files::*, *};
use mac_filesystem_interface::{self as fs, Arguments, Operation};
use mac_module::ClassId;
use spinning_top::Spinlock;

use crate::drivers::{OwnedJob, jobs::Failure};

const MAX_WAITERS: usize = crate::toolbox::handles::Kind::Threads.capacity();
static FLIGHTS: Spinlock<Vec<Flight>> = Spinlock::new(Vec::new());
static REAP: AtomicBool = AtomicBool::new(false);
#[derive(Clone, Copy, PartialEq, Eq)]
struct Key
{
    operation: u32,
    volume:    u32,
    index:     u32,
    cnid:      u32
}
enum Step
{
    Count,
    Mount(u32),
    Unmount(u32),
    Catalog
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
    owner:     u64,
    key:       Key,
    provider:  u32,
    step:      Step,
    job:       OwnedJob,
    abandoned: bool
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
    Unmounted
}

fn files<T>(call: impl FnOnce(&mut super::Manager) -> Result<T, Status>) -> Result<T, Status>
{
    crate::kernel::with_files(call).map_err(crate::state::AccessError::status)?
}
fn submit(provider: u32, op: Operation, volume: u32, source: u64) -> Result<OwnedJob, Status>
{
    submit_args(provider,
                op,
                Arguments { volume,
                            auxiliary: source,
                            ..Default::default() })
}
fn submit_args(provider: u32, op: Operation, args: Arguments<'_>) -> Result<OwnedJob, Status>
{
    let mut bytes = Vec::new();
    let length = fs::HEADER_BYTES + args.payload.len();
    bytes.try_reserve_exact(length).map_err(|_| NO_MEMORY)?;
    bytes.resize(length, 0);
    args.encode(&mut bytes).map_err(|_| BAD_ARGUMENT)?;
    let size = match op
    {
        Operation::Exports => 4,
        Operation::Mount => fs::MOUNT_BYTES + fs::METADATA_BYTES + 510,
        Operation::Catalog => fs::METADATA_BYTES + 510,
        Operation::Scan => 4 + fs::METADATA_BYTES + 510,
        _ => 0
    };
    crate::drivers::submit_background_to(provider,
                                         ClassId(fs::CLASS),
                                         fs::VERSION,
                                         op as u32,
                                         bytes,
                                         size)
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
    match submit(flight.provider, Operation::Unmount, volume, 0)
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
fn retire(volume: u32) -> Result<Reply, Status>
{
    files(|manager| manager.retire_provider_mount(volume)).map(|()| Reply::Unmounted)
}
fn finish_read(volume: u32, result: Result<CatalogInfo, Status>) -> Progress
{
    let released = files(|manager| manager.end_provider_read(volume));
    Progress::Done(released.and(result).map(Reply::Entry))
}
fn continue_read(mut flight: Flight, operation: Operation, args: Arguments<'_>) -> Progress
{
    match submit_args(flight.provider, operation, args)
    {
        Ok(job) =>
        {
            flight.job = job;
            Progress::Pending(flight)
        }
        Err(error) => finish_read(args.volume, Err(error))
    }
}
fn advance(mut flight: Flight) -> Progress
{
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
        Step::Catalog { volume,
                        node } =>
        {
            let result = result.and_then(|bytes| super::catalog(volume, &bytes, Some(node), None));
            finish_read(volume, result)
        }
        Step::Parent { volume,
                       node } =>
        {
            let result = result.and_then(|bytes| super::catalog(volume, &bytes, Some(node), None));
            match result
            {
                Ok(entry) if !flight.abandoned =>
                {
                    let parent = if entry.id == ROOT_ID { ROOT_ID } else { entry.parent };
                    if parent == entry.id
                    {
                        return finish_read(volume, Ok(entry));
                    }
                    flight.step = Step::Catalog { volume,
                                                  node: parent };
                    continue_read(flight,
                                  Operation::Catalog,
                                  Arguments { volume,
                                              node: parent,
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
                                   let bytes = bytes.as_slice().try_into().map_err(|_| BAD_ABI)?;
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
                return Progress::Done(retire(volume).and(Err(OPERATION_CANCELLED)));
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
                Ok(bytes) if bytes.is_empty() => Progress::Done(retire(volume)),
                Err(NOT_FOUND) => Progress::Done(retire(volume)),
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
            if matches!(&result, Ok(bytes) if bytes.is_empty()) || matches!(result, Err(NOT_FOUND))
            {
                Progress::Done(retire(volume).and(Err(status)))
            }
            else
            {
                let _ = files(|manager| manager.quarantine_provider_mount(volume));
                Progress::Done(Err(ROLLBACK_FAILED))
            }
        }
    }
}

fn reply(call: &mut FileCall, result: Result<Reply, Status>) -> Status
{
    match result
    {
        Ok(Reply::Count(count)) =>
        {
            call.index = count;
            OK
        }
        Ok(Reply::Mounted(info)) =>
        {
            call.volume = info.id;
            call.volume_info = info;
            OK
        }
        Ok(Reply::Unmounted) => OK,
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
        flight.abandoned = true;
        let _ = flight.job.cancel();
        REAP.store(true, Ordering::Release);
    }
    flights.push(flight); // capacity was reserved before any mount side effect
    result.err().unwrap_or(SERVICE_BUSY)
}

pub(crate) fn request(owner: u64, call: &mut FileCall) -> Status
{
    let thread = crate::context::current_thread().0;
    if thread <= 1
    {
        return WRONG_THREAD;
    }
    let key = Key { operation: call.operation,
                    volume:    call.volume,
                    index:     call.index,
                    cnid:      call.cnid };
    let mut flights = FLIGHTS.lock();
    if let Some(at) = flights.iter().position(|f| f.thread == thread)
    {
        if flights[at].key != key || flights[at].owner != owner || flights[at].abandoned
        {
            return BAD_ABI;
        }
        let flight = flights.swap_remove(at);
        drop(flights);
        return match advance(flight)
        {
            Progress::Done(result) => reply(call, result),
            Progress::Pending(flight) => waiting(flight)
        };
    }
    if flights.len() == MAX_WAITERS
    {
        return NO_MEMORY;
    }
    if flights.capacity() < MAX_WAITERS
    {
        let additional = MAX_WAITERS - flights.len();
        if flights.try_reserve_exact(additional).is_err()
        {
            return NO_MEMORY;
        }
    }
    drop(flights);
    if matches!(call.operation, CATALOG | PARENT | DIRECTORY_ENTRY)
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
             else
             {
                 Step::Catalog { volume,
                                 node }
             },
             Operation::Catalog,
             Arguments { volume,
                         node,
                         ..Default::default() })
        };
        let job = match submit_args(provider, operation, args)
        {
            Ok(job) => job,
            Err(error) =>
            {
                let _ = files(|manager| manager.end_provider_read(volume));
                return error;
            }
        };
        return waiting(Flight { thread,
                                owner,
                                key,
                                provider,
                                step,
                                job,
                                abandoned: false });
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
                return reply(call, Ok(Reply::Count(0)));
            }
            Err(error) => return error
        }
    };
    let (step, operation, volume, source) = match call.operation
    {
        HOST_EXPORT_COUNT => (Step::Count, Operation::Exports, 0, 0),
        MOUNT_HOST_EXPORT =>
        {
            match files(|manager| manager.existing_provider_mount(provider, u64::from(call.index)))
            {
                Ok(Some(info)) => return reply(call, Ok(Reply::Mounted(info))),
                Ok(None) => (),
                Err(error) => return error
            }
            let id = match files(|manager| {
                      manager.reserve_provider_mount(provider, u64::from(call.index))
                  })
            {
                Ok(id) => id,
                Err(error) => return error
            };
            (Step::Mount(id), Operation::Mount, id, u64::from(call.index))
        }
        UNMOUNT_VOLUME => (Step::Unmount(call.volume), Operation::Unmount, call.volume, 0),
        _ => return BAD_ARGUMENT
    };
    let job = match submit(provider, operation, volume, source)
    {
        Ok(job) => job,
        Err(error) =>
        {
            if operation == Operation::Mount
            {
                let _ = files(|manager| manager.retire_provider_mount(volume));
            }
            else if operation == Operation::Unmount
            {
                let _ = files(|manager| manager.quarantine_provider_mount(volume));
            }
            return error;
        }
    };
    waiting(Flight { thread,
                     owner,
                     key,
                     provider,
                     step,
                     job,
                     abandoned: false })
}

/// Thread exit only requests cleanup. The resident record retains buffers,
/// provider IDs and actual completion until cancellation has been acknowledged.
pub(crate) fn abandon_thread(thread: u64)
{
    let mut flights = FLIGHTS.lock();
    if let Some(flight) = flights.iter_mut().find(|flight| flight.thread == thread)
    {
        flight.abandoned = true;
        let _ = flight.job.cancel();
        REAP.store(true, Ordering::Release);
    }
}
pub(crate) fn pump()
{
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
    if let Progress::Pending(flight) = advance(flight)
    {
        FLIGHTS.lock().push(flight);
    }
}
