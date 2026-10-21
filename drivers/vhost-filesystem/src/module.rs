//! Bounded callback engine for a serialized FSYS provider. Move the client out
//! of its short state guard before polling a future: lower broker callbacks may
//! reenter resident services and must never inherit this guard or caller slices.
use alloc::{boxed::Box, vec::Vec};
use core::{future::Future,
           pin::Pin,
           task::{Context, Poll, Waker}};

use mac_driver::{Completion, Error, Phase, Request};
use mac_driver_alloc::Reservation;
use mac_filesystem_interface::{self as fs, Arguments, Operation};
use spinning_top::Spinlock;

use crate::{Cause, Filesystem, Result,
            transport::{Cancellation, Channel, DirectoryRpc}};
struct Finished<C: Channel>
{
    client: Filesystem<DirectoryRpc<C>>,
    result: Result<Vec<u8>>
}
// Only one operation exists per serialized provider. Keep its completed client
// inline: boxing here would allocate after a host mutation was acknowledged.
#[allow(clippy::large_enum_variant)]
enum Progress<C: Channel>
{
    Polling(Pin<Box<dyn Future<Output = Finished<C>> + Send>>),
    Ready(Finished<C>)
}
struct Active<C: Channel>
{
    ticket:    u32,
    operation: u32,
    progress:  Progress<C>
}
struct State<C: Channel>
{
    idle:         Option<Filesystem<DirectoryRpc<C>>>,
    active:       Option<Active<C>>,
    cancellation: Option<Cancellation>,
    running:      bool,
    quiescing:    bool
}
pub struct Service<C: Channel + 'static>
{
    state: Spinlock<State<C>>
}
impl<C: Channel + 'static> Default for Service<C>
{
    fn default() -> Self
    {
        Self::new()
    }
}
impl<C: Channel + 'static> Service<C>
{
    pub const fn new() -> Self
    {
        Self { state: Spinlock::new(State { idle:         None,
                                            active:       None,
                                            cancellation: None,
                                            running:      false,
                                            quiescing:    false }) }
    }

    pub fn initialize(&self, channel: C) -> mac_driver::Result<()>
    {
        let mut state = self.state.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
        if state.idle.is_some() || state.active.is_some() || state.running || state.quiescing
        {
            return Err(Error(mac_abi::SERVICE_BUSY));
        }
        let cancellation = Cancellation::default();
        state.idle = Some(Filesystem::new(DirectoryRpc::new(channel, cancellation.clone())));
        state.cancellation = Some(cancellation);
        Ok(())
    }

    pub fn request(&self, request: Request<'_>, output: &mut [u8])
                   -> mac_driver::Result<Completion>
    {
        if request.class.0 != fs::CLASS
           || request.ticket == 0
           || request.input.len() > fs::MAX_REQUEST
           || output.len() > fs::MAX_REPLY
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        // Validate before taking ownership; a malformed POLL cannot cancel or
        // replace the live operation, and a small START cannot mutate storage.
        if request.phase != Phase::Start && !request.input.is_empty()
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        let mut state = self.state.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
        if state.running
        {
            return Err(Error(mac_abi::SERVICE_BUSY));
        }
        let mut active = if request.phase == Phase::Start
        {
            if state.active.is_some()
            {
                return Err(Error(mac_abi::SERVICE_BUSY));
            }
            let operation =
                Operation::try_from(request.operation).map_err(|_| Error(mac_abi::NOT_SUPPORTED))?;
            if state.quiescing
               && !matches!(operation,
                            Operation::Close | Operation::AbortWrite | Operation::Unmount)
            {
                return Err(Error(mac_abi::SERVICE_BUSY));
            }
            let args = Arguments::decode(request.input).map_err(|_| Error(mac_abi::BAD_ARGUMENT))?;
            if output.len() < reply_capacity(operation, args)?
            {
                return Err(Error(mac_abi::BAD_ARGUMENT));
            }
            let cancellation = state.cancellation.clone().ok_or(Error(mac_abi::NOT_SUPPORTED))?;
            let mut input = Vec::new();
            let capacity = if matches!(operation, Operation::FinishWrite | Operation::AbortWrite)
            {
                request.input.len().max(fs::MAX_ABORT_RECEIPT_BYTES)
            }
            else
            {
                request.input.len()
            };
            input.try_reserve_exact(capacity).map_err(|_| Error(mac_abi::NO_MEMORY))?;
            input.extend_from_slice(request.input);
            // The reservation's future type is inferred from write below. OOM
            // occurs before the client moves, leaving every live lease owned.
            let storage = Reservation::try_new()?;
            let client = state.idle.take().ok_or(Error(mac_abi::NOT_SUPPORTED))?;
            state.running = true;
            drop(state);
            let future = storage.write(run(client, request.operation, input, cancellation));
            Active { ticket:    request.ticket,
                     operation: request.operation,
                     progress:  Progress::Polling(Box::into_pin(future)) }
        }
        else
        {
            let active = state.active.as_ref().ok_or(Error(mac_abi::NOT_FOUND))?;
            if active.ticket != request.ticket || active.operation != request.operation
            {
                return Err(Error(mac_abi::NOT_FOUND));
            }
            if request.phase == Phase::Cancel
            {
                state.cancellation.as_ref().unwrap().cancel();
            }
            let active = state.active.take().unwrap();
            state.running = true;
            drop(state);
            active
        };
        if let Progress::Polling(future) = &mut active.progress
        {
            // No state, registry, File Manager or caller-buffer borrow survives
            // into this poll. Exactly one bounded operation poll per callback.
            if let Poll::Ready(finished) =
                future.as_mut().poll(&mut Context::from_waker(Waker::noop()))
            {
                active.progress = Progress::Ready(finished);
            }
        }
        let mut state = self.state.lock();
        state.running = false;
        if let Progress::Ready(finished) = &active.progress
        {
            // Retain a completed result if a POLL temporarily supplies less
            // space. Never throw away an acknowledged mutation on cancellation.
            let required = finished.result.as_ref().map_or(0, |bytes| bytes.len());
            if required <= output.len()
            {
                let Progress::Ready(finished) = active.progress
                else
                {
                    unreachable!()
                };
                state.idle = Some(finished.client);
                state.cancellation.as_ref().unwrap().reset();
                return match finished.result
                {
                    Ok(bytes) =>
                    {
                        output[..bytes.len()].copy_from_slice(&bytes);
                        Ok(Completion::Complete(bytes.len()))
                    }
                    Err(error) => Err(Error(status(error)))
                };
            }
        }
        state.active = Some(active);
        Ok(Completion::Pending)
    }

    pub fn quiesce(&self) -> mac_driver::Result<()>
    {
        let mut state = self.state.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
        state.quiescing = true;
        if (state.running || state.active.is_some())
           && let Some(cancellation) = &state.cancellation
        {
            cancellation.cancel();
        }
        if state.running
           || state.active.is_some()
           || state.idle
                   .as_mut()
                   .is_none_or(|client| !client.quiescent() || client.transport().is_pending())
        {
            return Err(Error(mac_abi::SERVICE_BUSY));
        }
        Ok(())
    }
}
fn reply_capacity(op: Operation, a: Arguments<'_>) -> mac_driver::Result<usize>
{
    Ok(match op
    {
        Operation::Mount => fs::MAX_MOUNT_BYTES,
        Operation::Catalog
        | Operation::Lookup
        | Operation::BeginCreate
        | Operation::BeginReplace => fs::MAX_METADATA_BYTES,
        Operation::Scan => 4 + fs::MAX_METADATA_BYTES,
        Operation::FinishWrite => fs::MAX_WRITE_RECEIPT_BYTES,
        Operation::AbortWrite => fs::MAX_ABORT_RECEIPT_BYTES,
        Operation::Read =>
        {
            usize::try_from(a.length).ok()
                                     .filter(|n| *n <= fs::MAX_TRANSFER)
                                     .ok_or(Error(mac_abi::BAD_ARGUMENT))?
        }
        Operation::Open => 16,
        Operation::Exports => 4,
        _ => 0
    })
}
async fn run<C: Channel + 'static>(mut client: Filesystem<DirectoryRpc<C>>, operation: u32,
                                   mut input: Vec<u8>, cancellation: Cancellation)
                                   -> Finished<C>
{
    // An orphan from a failed earlier operation must remain owned and be
    // cleaned before admitting another mutation. Success is not converted to a
    // failure afterwards merely because unrelated deferred cleanup failed.
    let mut result = match client.cleanup_orphans().await
    {
        Ok(()) => client.execute(operation, &input).await,
        Err(error) => Err(error)
    };
    let cancelled = cancellation.begin_cleanup();
    if cancelled
    {
        // Finish the active future before cleanup. Its lower RPC has either
        // acknowledged its actual outcome or retained its own retryable state.
        let a = Arguments::decode(&input).expect("START validated the owned request");
        let (volume, token) = (a.volume, a.token);
        let op = Operation::try_from(operation).expect("START validated operation");
        let cleanup = match op
        {
            Operation::BeginCreate
            | Operation::BeginReplace
            | Operation::WriteChunk
            | Operation::FinishWrite
            | Operation::AbortWrite
                if client.has_write(volume, token) =>
            {
                let node = client.write_node(volume, token).unwrap();
                match client.abort_write(volume, token).await
                {
                    Ok(published) =>
                    {
                        if op == Operation::AbortWrite || published && op == Operation::FinishWrite
                        {
                            // Reuse storage reserved before START. A commit
                            // ACK survives cancellation even if subsequent
                            // metadata or cleanup was interrupted.
                            if op == Operation::AbortWrite
                            {
                                input.resize(fs::MAX_ABORT_RECEIPT_BYTES, 0);
                                result = client.encode_abort_receipt(volume, node, published,
                                                                     &mut input)
                                               .map(|count| {
                                                   input.truncate(count);
                                                   core::mem::take(&mut input)
                                               });
                            }
                            else
                            {
                                input.resize(fs::MAX_WRITE_RECEIPT_BYTES, 0);
                                result = client.encode_receipt(volume, node, &mut input)
                                               .map(|count| {
                                                   input.truncate(count);
                                                   core::mem::take(&mut input)
                                               });
                            }
                        }
                        else
                        {
                            result = Err(Cause::Cancelled.into());
                        }
                        Ok(())
                    }
                    Err(error) => Err(error)
                }
            }
            Operation::Open if result.is_ok() =>
            {
                client.close_file(volume, token)
                      .await
                      .map(|()| result = Err(Cause::Cancelled.into()))
            }
            Operation::Mount if client.has_mount(volume) =>
            {
                client.unmount(volume).await.map(|()| result = Err(Cause::Cancelled.into()))
            }
            _ => Ok(())
        };
        if let Err(error) = cleanup
        {
            result = Err(error);
        }
    }
    // The actual successful result takes precedence. The capability ledger
    // still owns deferred cleanup; the next START retries it, and quiesce
    // remains busy until it succeeds.
    if let Err(cleanup) = client.cleanup_orphans().await
       && let Err(error) = &mut result
    {
        error.cleanup = Some(cleanup.cause);
    }
    cancellation.reset();
    Finished { client,
               result }
}
fn status(error: crate::Error) -> i32
{
    if error.cleanup.is_some()
    {
        return mac_abi::ROLLBACK_FAILED;
    }
    match error.cause
    {
        Cause::Host(wire) =>
        {
            match wire
            {
                mac_host_protocol::status::READ_ONLY => mac_abi::READ_ONLY,
                mac_host_protocol::status::NOT_FOUND => mac_abi::NOT_FOUND,
                mac_host_protocol::status::STALE => mac_abi::SCAN_STALE,
                mac_host_protocol::status::INVALID => mac_abi::BAD_ARGUMENT,
                mac_host_protocol::status::UNAVAILABLE => mac_abi::STORAGE_UNAVAILABLE,
                mac_host_protocol::status::UNSUPPORTED => mac_abi::NOT_SUPPORTED,
                mac_host_protocol::status::LIMIT => mac_abi::NO_MEMORY,
                mac_host_protocol::status::CONFLICT => mac_abi::ALREADY_EXISTS,
                mac_host_protocol::status::BUSY => mac_abi::SERVICE_BUSY,
                _ => mac_abi::IO_ERROR
            }
        }
        Cause::Transport(status) => status,
        Cause::Invalid => mac_abi::BAD_ARGUMENT,
        Cause::NotFound => mac_abi::NOT_FOUND,
        Cause::Stale => mac_abi::SCAN_STALE,
        Cause::Conflict => mac_abi::ALREADY_EXISTS,
        Cause::ReadOnly => mac_abi::READ_ONLY,
        Cause::NoMemory => mac_abi::NO_MEMORY,
        Cause::Busy => mac_abi::SERVICE_BUSY,
        Cause::Unsupported => mac_abi::NOT_SUPPORTED,
        Cause::Cancelled => mac_abi::OPERATION_CANCELLED,
        Cause::Format(mac_fork_file::Error::NoMemory) => mac_abi::NO_MEMORY,
        Cause::Format(mac_fork_file::Error::Stale) => mac_abi::SCAN_STALE,
        Cause::Format(mac_fork_file::Error::Version) => mac_abi::BAD_ABI,
        Cause::Format(mac_fork_file::Error::Io) => mac_abi::IO_ERROR,
        Cause::Format(_) => mac_abi::CORRUPT_DATA
    }
}
