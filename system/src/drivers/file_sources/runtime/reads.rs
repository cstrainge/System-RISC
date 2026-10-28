//! A source read owns its authenticated plan and lower job. No caller buffer,
//! Source Manager guard or File Manager guard survives a pending operation.
#![forbid(unsafe_code)]
use alloc::{collections::VecDeque, vec::Vec};
use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::{BAD_ABI, OPERATION_CANCELLED};
use mac_filesystem_interface::{self as fs, Arguments as FileArguments, Operation as FileOperation};

use super::*;
use crate::drivers::{OwnedJob, jobs::Failure};
struct Flight
{
    caller:   u64,
    plan:     Read<Backing>,
    args:     FileArguments<'static>,
    job:      OwnedJob,
    done:     Option<Result<Vec<u8>, Status>>,
    cancel:   bool,
    orphaned: bool
}
static FLIGHTS: Spinlock<VecDeque<Flight>> = Spinlock::new(VecDeque::new());
static ORPHANS: AtomicBool = AtomicBool::new(false);
pub(super) fn submit(subject: &Subject, provider: u64, op: FileOperation, args: FileArguments<'_>,
                     capacity: usize)
                     -> Result<OwnedJob, Status>
{
    crate::allocator::with_target(subject.allocation().clone(), || {
        let mut input = Vec::new();
        let size = fs::HEADER_BYTES.checked_add(args.payload.len()).ok_or(NO_MEMORY)?;
        input.try_reserve_exact(size).map_err(|_| NO_MEMORY)?;
        input.resize(size, 0);
        args.encode(&mut input).map_err(|_| BAD_ARGUMENT)?;
        crate::drivers::submit_background_to(subject,
                                             provider,
                                             mac_module::ClassId(fs::CLASS),
                                             fs::VERSION,
                                             op as u32,
                                             input,
                                             capacity)
    })
}

fn retained(flight: Flight)
{
    InvocationGate::run(|| {
        let orphaned = flight.orphaned;
        let mut flights = FLIGHTS.lock();
        flights.push_back(flight);
        if orphaned
        {
            ORPHANS.store(true, Ordering::Release);
            wake();
        }
    });
}
fn acknowledged(plan: Read<Backing>) -> Result<(), Status>
{
    with(|sources| {
        let result = sources.finish_read(plan).map_err(|refusal| {
                                                  serial!("BACKING_READ_COMPLETION_REFUSED \
                                                           {refusal}");
                                                  {
                                                      let (cause, plan) = refusal.into_parts();
                                                      drop(plan);
                                                      cause
                                                  }
                                              });
        changed(sources);
        result
    })?
}
fn begin(call: ResidentCall<'_>, output: &mut [u8]) -> Result<ResidentCompletion, Status>
{
    let args = Arguments::decode(Operation::Read, call.input).map_err(|_| BAD_ARGUMENT)?;
    if output.len() < args.length as usize
    {
        return Err(BAD_ARGUMENT);
    }
    let owner = crate::drivers::module_authority(call.caller)?;
    let plan = with(|sources| {
        let needed = sources.records.len();
        InvocationGate::run(|| {
            let mut flights = FLIGHTS.lock();
            if flights.iter().any(|flight| {
                                 flight.caller == call.caller && flight.plan.ticket() == call.ticket
                             })
            {
                return Err(SERVICE_BUSY);
            }
            let reserve = needed.saturating_sub(flights.len());
            InvocationGate::with_system_allocation(|| flights.try_reserve(reserve)).map_err(|_| {
                                                                                       NO_MEMORY
                                                                                   })
        })?;
        sources.begin_read(&owner, args.source, call.ticket, args.offset, args.length as usize)
    })??;
    let backing = &plan.backing;
    match files(|files| files.source_mark_provider(backing.subject.scope(), backing.file))
    {
        Ok(None) =>
        {
            let result = files(|files| {
                files.read_block_source(backing.subject.scope(),
                                        backing.file,
                                        plan.offset,
                                        &mut output[..plan.length])
            });
            acknowledged(plan)?;
            result.map(ResidentCompletion::Complete)
        }
        Ok(Some(_)) =>
        {
            let prepared = files(|files| {
                files.begin_provider_source_read(backing.subject.scope(),
                                                 backing.file,
                                                 plan.offset,
                                                 plan.length)
            });
            let (provider, file_args) = match prepared
            {
                Ok(prepared) => prepared,
                Err(error) =>
                {
                    acknowledged(plan)?;
                    return Err(error);
                }
            };
            // The actual incoming operation owns this read's machine-rate
            // work. The module source scope owns the persistent backing mark.
            let job = crate::context::io_subject().ok_or(mac_abi::WRONG_THREAD)
                                                  .and_then(|subject| {
                                                      submit(&subject,
                                                             provider,
                                                             FileOperation::Read,
                                                             file_args,
                                                             plan.length)
                                                  });
            let job = match job
            {
                Ok(job) => job,
                Err(error) =>
                {
                    // No lower request was published. Undo local read tracking
                    // before acknowledging the original plan, keeping its scope
                    // pinned through both operations.
                    files(|files| {
                        files.finish_provider_source_read(backing.subject.scope(),
                                                          backing.file,
                                                          file_args.offset,
                                                          file_args.length,
                                                          None)
                    })?;
                    acknowledged(plan)?;
                    return Err(error);
                }
            };
            retained(Flight { caller: call.caller,
                              plan,
                              args: file_args,
                              job,
                              done: None,
                              cancel: false,
                              orphaned: false });
            Ok(ResidentCompletion::Pending)
        }
        Err(error) =>
        {
            acknowledged(plan)?;
            Err(error)
        }
    }
}
fn advance(mut flight: Flight, mut output: Option<&mut [u8]>)
           -> Result<ResidentCompletion, Status>
{
    if flight.cancel && flight.done.is_none()
    {
        let _ = flight.job.cancel();
    }
    if flight.done.is_none()
    {
        match flight.job.poll()
        {
            Ok(None) => (),
            Ok(Some(done)) =>
            {
                flight.done = Some(done.result.map_err(|failure| {
                                                  match failure
                                                  {
                                                      Failure::Service(status) => status,
                                                      Failure::Cancelled => OPERATION_CANCELLED
                                                  }
                                              }))
            }
            Err(error) =>
            {
                let _ = flight.job.cancel();
                with(|sources| sources.quarantine_read(&flight.plan))??;
                report(flight.plan.receipt.id(), error, "lower read completion");
                RETRY_SAMPLES.store(8, Ordering::Release);
                flight.orphaned = true;
                flight.cancel = true;
                retained(flight);
                return Err(ROLLBACK_FAILED);
            }
        }
    }
    if let Some(result) = &flight.done
    {
        let oversized = result.as_ref().is_ok_and(|bytes| bytes.len() > flight.plan.length);
        let fits = flight.cancel
                   || oversized
                   || result.as_ref()
                            .map_or(true, |bytes| {
                                output.as_ref().is_some_and(|output| bytes.len() <= output.len())
                            });
        if fits
        {
            let delivered =
                if flight.cancel || oversized { None } else { result.as_ref().ok().map(Vec::len) };
            match files(|files| {
                      files.finish_provider_source_read(flight.plan.backing.subject.scope(),
                                                        flight.plan.backing.file,
                                                        flight.args.offset,
                                                        flight.args.length,
                                                        delivered)
                  })
            {
                Ok(()) => (),
                Err(SERVICE_BUSY) =>
                {
                    retained(flight);
                    return Ok(ResidentCompletion::Pending);
                }
                Err(error) =>
                {
                    with(|sources| sources.quarantine_read(&flight.plan))??;
                    report(flight.plan.receipt.id(), error, "read acknowledgement bookkeeping");
                    RETRY_SAMPLES.store(8, Ordering::Release);
                    flight.orphaned = true;
                    flight.cancel = true;
                    retained(flight);
                    return Err(ROLLBACK_FAILED);
                }
            }
            let Some(result) = flight.done.take()
            else
            {
                retained(flight);
                return Err(BAD_ABI);
            };
            acknowledged(flight.plan)?;
            if flight.cancel
            {
                return Err(OPERATION_CANCELLED);
            }
            if oversized
            {
                return Err(BAD_ABI);
            }
            let bytes = result?;
            let target = output.as_mut().ok_or(BAD_ABI)?.get_mut(..bytes.len()).ok_or(BAD_ABI)?;
            target.copy_from_slice(&bytes);
            return Ok(ResidentCompletion::Complete(bytes.len()));
        }
    }
    retained(flight);
    Ok(ResidentCompletion::Pending)
}
pub(super) fn request(call: ResidentCall<'_>, output: &mut [u8])
                      -> Result<ResidentCompletion, Status>
{
    if call.phase == abi::START
    {
        return begin(call, output);
    }
    if ![abi::POLL, abi::CANCEL].contains(&call.phase) || !call.input.is_empty()
    {
        return Err(BAD_ARGUMENT);
    }
    let mut flight = InvocationGate::run(|| {
        let mut flights = FLIGHTS.lock();
        let at = flights.iter()
                        .position(|flight| {
                            flight.caller == call.caller && flight.plan.ticket() == call.ticket
                        })
                        .ok_or(NOT_FOUND)?;
        flights.remove(at).ok_or(NOT_FOUND)
    })?;
    if call.phase == abi::CANCEL
    {
        flight.cancel = true;
    }
    advance(flight, Some(output))
}
pub(super) fn has_orphans() -> bool
{
    ORPHANS.load(Ordering::Acquire)
}
/// The System retains uncertain flights after their caller is gone. A real
/// lower ACK may still settle them; time alone never acknowledges their I/O.
pub(super) fn pump() -> bool
{
    if !has_orphans()
    {
        return false;
    }
    let flight = InvocationGate::run(|| {
        let mut flights = FLIGHTS.lock();
        let Some(at) = flights.iter().position(|flight| flight.orphaned)
        else
        {
            ORPHANS.store(false, Ordering::Release);
            return None;
        };
        // Preserve FIFO order: swapping the last item into the first one's
        // place can make two stalled reads starve every flight between them.
        flights.remove(at)
    });
    if let Some(flight) = flight
    {
        let _ = advance(flight, None);
        true
    }
    else
    {
        false
    }
}
