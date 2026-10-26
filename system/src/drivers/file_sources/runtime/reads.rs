//! A source ticket owns a lower FSYS job, never caller buffers or a Files guard.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{BAD_ABI, OPERATION_CANCELLED};
use mac_filesystem_interface::{self as fs, Arguments as FileArguments, Operation as FileOperation};

use super::*;
use crate::drivers::{OwnedJob, jobs::Failure};
struct Flight
{
    caller: u64,
    ticket: u32,
    source: u32,
    plan:   Read<Backing>,
    args:   FileArguments<'static>,
    job:    OwnedJob,
    done:   Option<Result<Vec<u8>, Status>>,
    cancel: bool
}
static FLIGHTS: Spinlock<Vec<Flight>> = Spinlock::new(Vec::new());

pub(super) fn submit(subject: &crate::drivers::jobs::subjects::Subject, provider: u64,
                     op: FileOperation, args: FileArguments<'_>, capacity: usize)
                     -> Result<OwnedJob, Status>
{
    let mut input = Vec::new();
    let size = fs::HEADER_BYTES + args.payload.len();
    input.try_reserve_exact(size).map_err(|_| mac_abi::NO_MEMORY)?;
    input.resize(size, 0);
    args.encode(&mut input).map_err(|_| BAD_ARGUMENT)?;
    crate::drivers::submit_background_to(subject,
                                         provider,
                                         mac_module::ClassId(fs::CLASS),
                                         fs::VERSION,
                                         op as u32,
                                         input,
                                         capacity)
}
fn begin(call: ResidentCall<'_>, output: &mut [u8]) -> Result<ResidentCompletion, Status>
{
    let args = Arguments::decode(Operation::Read, call.input).map_err(|_| BAD_ARGUMENT)?;
    if output.len() < args.length as usize
    {
        return Err(BAD_ARGUMENT);
    }
    {
        let mut flights = FLIGHTS.lock();
        if flights.len() == Kind::FileSources.capacity()
        {
            return Err(mac_abi::NO_MEMORY);
        }
        if flights.iter().any(|f| f.caller == call.caller && f.ticket == call.ticket)
        {
            return Err(SERVICE_BUSY);
        }
        let reserve = Kind::FileSources.capacity().saturating_sub(flights.len());
        flights.try_reserve_exact(reserve).map_err(|_| mac_abi::NO_MEMORY)?;
    }
    let plan =
        SOURCES.lock().begin_read(call.caller, args.source, call.ticket, args.offset,
                                   args.length as usize)?;
    let provider = files(|manager| manager.source_mark_provider(plan.backing.file));
    match provider
    {
        Ok(None) =>
        {
            let result = files(|manager| {
                manager.read_block_source(plan.backing.file,
                                          plan.offset,
                                          &mut output[..plan.length])
            });
            SOURCES.lock().finish_read(call.caller, args.source, call.ticket)?;
            result.map(ResidentCompletion::Complete)
        }
        Ok(Some(_)) =>
        {
            let prepared = files(|manager| {
                manager.begin_provider_source_read(plan.backing.file, plan.offset, plan.length)
            });
            let (provider, file_args) = match prepared
            {
                Ok(prepared) => prepared,
                Err(error) =>
                {
                    SOURCES.lock().finish_read(call.caller, args.source, call.ticket)?;
                    return Err(error);
                }
            };
            // This read belongs to the incoming operation. The source itself
            // retains its module authority for independent close/cleanup, but
            // must not charge a client's stacked read to that module instead.
            let subject = crate::context::io_subject().ok_or(mac_abi::WRONG_THREAD);
            let job = match subject.and_then(|subject| {
                                       submit(&subject,
                                              provider,
                                              FileOperation::Read,
                                              file_args,
                                              plan.length)
                                   })
            {
                Ok(job) => job,
                Err(error) =>
                {
                    let finished = files(|manager| {
                        manager.finish_provider_source_read(plan.backing.file, file_args.offset,
                                                            file_args.length, None)
                    });
                    SOURCES.lock().finish_read(call.caller, args.source, call.ticket)?;
                    return finished.and(Err(error));
                }
            };
            FLIGHTS.lock().push(Flight { caller: call.caller,
                                         ticket: call.ticket,
                                         source: args.source,
                                         plan,
                                         args: file_args,
                                         job,
                                         done: None,
                                         cancel: false });
            Ok(ResidentCompletion::Pending)
        }
        Err(error) =>
        {
            SOURCES.lock().finish_read(call.caller, args.source, call.ticket)?;
            Err(error)
        }
    }
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
    let mut flights = FLIGHTS.lock();
    let at = flights.iter()
                    .position(|f| f.caller == call.caller && f.ticket == call.ticket)
                    .ok_or(mac_abi::NOT_FOUND)?;
    let mut flight = flights.swap_remove(at);
    drop(flights);
    if call.phase == abi::CANCEL
    {
        flight.cancel = true;
        if flight.done.is_none()
        {
            let _ = flight.job.cancel();
        }
    }
    if flight.done.is_none()
    {
        match flight.job.poll()
        {
            Ok(None) => (),
            Ok(Some(done)) =>
            {
                flight.done = Some(done.result.map_err(|f| {
                                                  match f
                                                  {
                                                      Failure::Service(s) => s,
                                                      Failure::Cancelled => OPERATION_CANCELLED
                                                  }
                                              }))
            }
            Err(_) =>
            {
                let _ = flight.job.cancel();
                let _ =
                    files(|manager| manager.quarantine_provider_source(flight.plan.backing.file));
                SOURCES.lock().quarantine_read(call.caller, flight.source, call.ticket)?;
                return Err(mac_abi::ROLLBACK_FAILED);
            }
        }
    }
    if let Some(result) = &flight.done
       && (flight.cancel || result.as_ref().map_or(true, |b| b.len() <= output.len()))
    {
        let result = flight.done.take().unwrap();
        let delivered = if flight.cancel { None } else { result.as_ref().ok().map(Vec::len) };
        let finished = files(|manager| {
            manager.finish_provider_source_read(flight.plan.backing.file, flight.args.offset,
                                                flight.args.length, delivered)
        });
        SOURCES.lock().finish_read(call.caller, flight.source, call.ticket)?;
        finished?;
        if flight.cancel
        {
            return Err(OPERATION_CANCELLED);
        }
        let bytes = result?;
        if bytes.len() > flight.plan.length
        {
            return Err(BAD_ABI);
        }
        output[..bytes.len()].copy_from_slice(&bytes);
        return Ok(ResidentCompletion::Complete(bytes.len()));
    }
    FLIGHTS.lock().push(flight);
    Ok(ResidentCompletion::Pending)
}
