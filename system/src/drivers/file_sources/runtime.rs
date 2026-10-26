//! Resident source coordinator. Close ownership stays in resident actors after
//! the client exits; common cleanup only queues intent while Registry is borrowed.
#![forbid(unsafe_code)]
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use mac_file_source_interface::{self as wire, Arguments, Operation};
use mac_module::{Offer, abi};
use spinning_top::Spinlock;

use super::*;
use crate::drivers::{OwnedJob, ResidentCall, ResidentCompletion, jobs::subjects::Subject};
mod reads;
#[derive(Clone)]
pub(crate) struct Backing
{
    pub(crate) file:    u32,
    pub(crate) subject: Subject
}
/// This plan and the actual close job live in resident File Manager state (or
/// the common-cleanup actor below), independently of an application's stack.
pub(crate) struct Closing
{
    id:      u64,
    plan:    Option<Close<Backing>>,
    done:    Option<Result<alloc::vec::Vec<u8>, Status>>,
    retired: bool
}
impl Closing
{
    pub(crate) fn has_reply(&self) -> bool
    {
        self.done.is_some()
    }
}
static SOURCES: spin::Once<Spinlock<Sources<Backing>>> = spin::Once::new();
static WORK: AtomicBool = AtomicBool::new(false);
static RETRY_SAMPLES: AtomicU8 = AtomicU8::new(0);
struct Cleanup
{
    closing: Closing,
    job:     Option<OwnedJob>
}
enum Task
{
    Start(Close<Backing>),
    Poll(Cleanup)
}
static CLEANUP: Spinlock<alloc::collections::VecDeque<Task>> =
    Spinlock::new(alloc::collections::VecDeque::new());
static ORPHAN_TURN: AtomicBool = AtomicBool::new(false);
fn with<R>(call: impl FnOnce(&mut Sources<Backing>) -> R) -> Result<R, Status>
{
    let state = SOURCES.get().ok_or(mac_abi::NOT_SUPPORTED)?;
    Ok(InvocationGate::run(|| call(&mut state.lock())))
}
fn enqueue(task: Task)
{
    InvocationGate::run(|| {
        let mut queue = CLEANUP.lock();
        queue.push_back(task);
        WORK.store(true, Ordering::Release);
    });
}
fn changed(sources: &mut Sources<Backing>)
{
    // This is metadata only. Start/poll work shares one FIFO, so one slow lower
    // provider cannot starve another source's newly admitted cleanup intent.
    loop
    {
        match sources.next_cleanup()
        {
            Ok(Some(plan)) => enqueue(Task::Start(plan)),
            Ok(None) => break,
            Err(error) =>
            {
                serial!("BACKING_CLEANUP_INTENT_REFUSED status={error}; original source remains \
                         retained");
                break;
            }
        }
    }
}
fn wake()
{
    // Serialize the set with the queue consumer's empty/clear operation.
    InvocationGate::run(|| {
        let _queue = CLEANUP.lock();
        WORK.store(true, Ordering::Release);
    });
}
pub(crate) fn files<T>(call: impl FnOnce(&mut crate::toolbox::files::Manager) -> Result<T, Status>)
                       -> Result<T, Status>
{
    crate::kernel::with_files(call).map_err(crate::state::AccessError::status)?
}
pub(crate) fn admit(provider: u64) -> Result<Subject, Status>
{
    let module = crate::drivers::provider_module(provider)?;
    let owner = crate::drivers::module_authority(module)?;
    crate::drivers::jobs::subjects::resident()?.admit(
        &crate::toolbox::handles::subject::Authority::Client(owner))
}
pub(crate) fn pin(subject: &Subject, handler: u64, provider: u64) -> Result<(), Status>
{
    let dependent = crate::drivers::provider_module(handler)?;
    crate::drivers::pin_backing_provider(dependent, provider, subject.scope())
}
fn publish(provider: u64, volume: u32, cnid: u32, backing: &Backing, length: u64)
           -> Result<u64, Status>
{
    let module = crate::drivers::provider_module(provider)?;
    let owner = crate::drivers::module_authority(module)?;
    with(|sources| {
        // Includes plans currently detached into resident I/O. No cleanup push
        // needs allocation even if more sources enter before those plans return.
        let needed = sources.records.len().checked_add(1).ok_or(NO_MEMORY)?;
        InvocationGate::run(|| {
            let mut cleanup = CLEANUP.lock();
            let reserve = needed.saturating_sub(cleanup.len());
            cleanup.try_reserve(reserve).map_err(|_| NO_MEMORY)
        })?;
        sources.publish(&owner,
                        Identity { volume,
                                   cnid,
                                   fork: mac_abi::files::DATA_FORK },
                        backing,
                        length)
    })?
}
pub(crate) fn find(provider: u64, volume: u32, cnid: u32) -> Result<Option<u64>, Status>
{
    let owner = crate::drivers::module_authority(crate::drivers::provider_module(provider)?)?;
    with(|sources| {
        sources.find(&owner,
                     Identity { volume,
                                cnid,
                                fork: mac_abi::files::DATA_FORK })
    })?
}
pub(crate) fn adopt(provider: u64, volume: u32, cnid: u32, subject: Subject, file: u32,
                    length: u64)
                    -> Result<u64, Status>
{
    let backing = Backing { file,
                            subject };
    publish(provider, volume, cnid, &backing, length)
}
pub(crate) fn lease(provider: u64, volume: u32, cnid: u32) -> Result<(u64, bool), Status>
{
    if let Some(source) = find(provider, volume, cnid)?
    {
        return Ok((source, false));
    }
    let subject = admit(provider)?;
    let (file, length) = files(|files| files.open_block_source(subject.scope(), volume, cnid))?;
    let backing = Backing { file,
                            subject };
    match publish(provider, volume, cnid, &backing, length)
    {
        Ok(source) => Ok((source, true)),
        Err(error) =>
        {
            files(|files| files.close_block_source(backing.subject.scope(), backing.file))?;
            Err(error)
        }
    }
}
fn unsubmitted(plan: Close<Backing>) -> Result<(), Status>
{
    with(|sources| {
        let result = sources.close_unsubmitted(plan).map_err(|refusal| {
                                                        serial!("BACKING_CLOSE_REFUSED {refusal}");
                                                        {
                                                            let (cause, plan) =
                                                                refusal.into_parts();
                                                            drop(plan);
                                                            cause
                                                        }
                                                    });
        changed(sources);
        result
    })?
}
fn report(source: u64, status: Status, stage: &'static str)
{
    let _ = with(|sources| {
        if let Some(at) = sources.at(source)
        {
            if sources.records[at].last_io_error != Some(status)
            {
                serial!("BACKING_SOURCE_REFUSED source={source} stage={stage} status={status}; \
                         original payload and cleanup authority remain retained");
                sources.records[at].last_io_error = Some(status);
            }
        }
    });
}
fn start(plan: Close<Backing>) -> Result<Option<(Closing, OwnedJob)>, Status>
{
    let backing = &plan.backing;
    let provider =
        match files(|files| files.source_mark_provider(backing.subject.scope(), backing.file))
        {
            Ok(provider) => provider,
            Err(error) =>
            {
                unsubmitted(plan)?;
                return Err(error);
            }
        };
    if provider.is_none()
    {
        if let Err(error) =
            files(|files| files.close_block_source(backing.subject.scope(), backing.file))
        {
            unsubmitted(plan)?;
            return Err(error);
        }
        return with(|sources| {
                   sources.finish_close(plan).map_err(|refusal| {
                                                 serial!("BACKING_CLOSE_REFUSED {refusal}");
                                                 {
                                                     let (cause, plan) = refusal.into_parts();
                                                     drop(plan);
                                                     cause
                                                 }
                                             })
               })?.map(|()| None);
    }
    // The mark stays borrowed/pinned across fallible queue admission. The
    // callback queues work only; it cannot execute the provider under a guard.
    let job = files(|files| {
        files.close_provider_source(backing.subject.scope(),
                                    backing.file,
                                    false,
                                    |provider, args| {
                                        reads::submit(&backing.subject,
                                                      provider,
                                                      mac_filesystem_interface::Operation::Close,
                                                      args,
                                                      0)
                                    })
    });
    match job
    {
        Ok(job) =>
        {
            Ok(Some((Closing { id:      plan.receipt.id(),
                               plan:    Some(plan),
                               done:    None,
                               retired: false },
                     job)))
        }
        Err(error) =>
        {
            unsubmitted(plan)?;
            Err(error)
        }
    }
}
pub(crate) fn begin_release(provider: u64, source: u64)
                            -> Result<Option<(Closing, OwnedJob)>, Status>
{
    let owner = crate::drivers::module_authority(crate::drivers::provider_module(provider)?)?;
    let plan = with(|sources| sources.begin_close(&owner, source))??;
    start(plan)
}
/// Actual lower result is retained through temporary File Manager refusal. It
/// is not polled/consumed twice, nor confused with a missing job after dequeue.
pub(crate) fn finish_release(close: &mut Closing,
                             result: Option<Result<alloc::vec::Vec<u8>, Status>>)
                             -> Result<bool, Status>
{
    if let Some(result) = result
    {
        close.done = Some(result);
    }
    let Some(result) = &close.done
    else
    {
        return Ok(false);
    };
    let Some(plan) = &close.plan
    else
    {
        return Ok(true);
    };
    let backing = &plan.backing;
    let ack = matches!(result, Ok(bytes) if bytes.is_empty()) || matches!(result, Err(NOT_FOUND));
    let terminal = if ack
    {
        if !close.retired
        {
            match files(|files| files.retire_provider_source(backing.subject.scope(), backing.file))
            {
                Ok(()) => close.retired = true,
                Err(SERVICE_BUSY) => return Ok(false),
                Err(error) =>
                {
                    with(|sources| sources.quarantine_close(plan))??;
                    return Err(error);
                }
            }
        }
        true
    }
    else
    {
        match files(|files| files.quarantine_provider_source(backing.subject.scope(), backing.file))
        {
            Ok(()) => (),
            Err(SERVICE_BUSY) => return Ok(false),
            Err(error) =>
            {
                with(|sources| sources.quarantine_close(plan))??;
                return Err(error);
            }
        }
        false
    };
    let Some(plan) = close.plan.take()
    else
    {
        return Err(BAD_ABI);
    };
    with(|sources| {
        let result = if terminal { sources.finish_close(plan) } else { sources.close_failed(plan) };
        changed(sources);
        result.map_err(|refusal| {
                  serial!("BACKING_CLOSE_REFUSED {refusal}");
                  {
                      let (cause, plan) = refusal.into_parts();
                      drop(plan);
                      cause
                  }
              })
    })??;
    if terminal { Ok(true) } else { Err(result.as_ref().err().copied().unwrap_or(BAD_ABI)) }
}
pub(crate) fn release(ticket: ReleasedResource, storage: bool) -> Result<(), Failure>
{
    let Some(state) = SOURCES.get()
    else
    {
        return Err(Failure::context_unavailable(ticket, "backing-file service"));
    };
    InvocationGate::run(|| {
        let mut sources = state.lock();
        let result = sources.release(ticket, storage);
        changed(&mut sources);
        result
    })
}
/// At most one close continuation per allowed maintenance sample. Empty work
/// costs an atomic load; failed admission backs off without a busy wait.
pub(crate) fn pump()
{
    if !WORK.load(Ordering::Acquire)
    {
        return;
    }
    if RETRY_SAMPLES.try_update(Ordering::AcqRel, Ordering::Acquire, |left| left.checked_sub(1))
                    .is_ok()
    {
        return;
    }
    if ORPHAN_TURN.fetch_xor(true, Ordering::AcqRel) && reads::pump()
    {
        return;
    }
    let task = InvocationGate::run(|| {
        let mut queue = CLEANUP.lock();
        let task = queue.pop_front();
        if task.is_none() && !reads::has_orphans()
        {
            WORK.store(false, Ordering::Release);
        }
        task
    });
    let mut flight = match task
    {
        Some(Task::Poll(flight)) => flight,
        Some(Task::Start(plan)) =>
        {
            let id = plan.receipt.id();
            match start(plan)
            {
                Ok(Some((closing, job))) =>
                {
                    Cleanup { closing,
                              job: Some(job) }
                }
                Ok(None) => return,
                Err(error) =>
                {
                    report(id, error, "queue admission");
                    RETRY_SAMPLES.store(8, Ordering::Release);
                    return;
                }
            }
        }
        None =>
        {
            reads::pump();
            return;
        }
    };
    if let Some(job) = &mut flight.job
    {
        match job.poll()
        {
            Ok(None) => (),
            Ok(Some(done)) =>
            {
                flight.closing.done = Some(done.result.map_err(|failure| {
                    match failure
                    {
                        crate::drivers::jobs::Failure::Service(status) => status,
                        crate::drivers::jobs::Failure::Cancelled => mac_abi::OPERATION_CANCELLED
                    }
                }));
                flight.job = None;
            }
            Err(error) =>
            {
                // No terminal evidence: retain both plan and job rather than
                // manufacture Close ACK or discard its cleanup capability.
                report(flight.closing.id, error, "lower close completion");
                RETRY_SAMPLES.store(8, Ordering::Release);
            }
        }
    }
    let finished = if flight.closing.has_reply()
    {
        finish_release(&mut flight.closing, None)
    }
    else
    {
        Ok(false)
    };
    match finished
    {
        Ok(true) => (),
        Ok(false) => enqueue(Task::Poll(flight)),
        Err(error) =>
        {
            report(flight.closing.id, error, "backing close");
            RETRY_SAMPLES.store(8, Ordering::Release);
        }
    }
}
pub(crate) fn initialize() -> Result<(), Status>
{
    let kinds = crate::toolbox::handles::registry(|registry| {
                    Ok((registry.get(lifetime::specification(false).kind)?,
                        registry.get(lifetime::specification(true).kind)?))
                }).map_err(|error| error.status())?
                  .map_err(|error| crate::toolbox::handles::registry_status(&error))?;
    SOURCES.try_call_once(|| Sources::new(kinds).map(Spinlock::new))?;
    crate::drivers::register_service(Offer { class:          wire::CLASS,
                                             version:        wire::VERSION,
                                             request_schema: 1,
                                             reply_schema:   1,
                                             max_request:    wire::ARGUMENT_BYTES as u32,
                                             max_reply:      wire::MAX_TRANSFER as u32,
                                             flags:          Offer::KERNEL_ONLY,
                                             capabilities:   0 },
                                     request)?;
    Ok(())
}
fn request(call: ResidentCall<'_>, output: &mut [u8]) -> Result<ResidentCompletion, Status>
{
    let operation = Operation::try_from(call.operation).map_err(|_| BAD_ARGUMENT)?;
    if operation == Operation::Read
    {
        return reads::request(call, output);
    }
    if call.phase != abi::START
    {
        return Err(BAD_ARGUMENT);
    }
    let args = Arguments::decode(operation, call.input).map_err(|_| BAD_ARGUMENT)?;
    let owner = crate::drivers::module_authority(call.caller)?;
    let info = with(|sources| sources.info(&owner, args.source))??;
    output.get_mut(..wire::INFO_BYTES).ok_or(BAD_ARGUMENT)?.copy_from_slice(&info.encode());
    Ok(ResidentCompletion::Complete(wire::INFO_BYTES))
}
