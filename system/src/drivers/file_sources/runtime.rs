//! Resident backing-file service. The initial adapter leases block-backed files;
//! provider-backed sources require the owned deferred-I/O adapter before use.
#![forbid(unsafe_code)]
use mac_file_source_interface::{self as wire, Arguments, Operation};
use mac_module::{Offer, abi};
use spinning_top::Spinlock;

use super::*;
use crate::drivers::{ResidentCall, ResidentCompletion};
static SOURCES: Spinlock<Sources<u32>> = Spinlock::new(Sources::new());
fn files<T>(call: impl FnOnce(&mut crate::toolbox::files::Manager) -> Result<T, Status>)
            -> Result<T, Status>
{
    crate::kernel::with_files(call).map_err(crate::state::AccessError::status)?
}
pub(crate) fn initialize() -> Result<(), Status>
{
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
pub(crate) fn lease(provider: u32, volume: u32, cnid: u32) -> Result<(u32, bool), Status>
{
    let owner = crate::drivers::provider_module(provider)?;
    let identity = Identity { volume,
                              cnid,
                              fork: mac_abi::files::DATA_FORK };
    if let Some(source) = SOURCES.lock().find(owner, identity)?
    {
        return Ok((source, false));
    }
    let (file, length) = files(|files| files.open_block_source(volume, cnid))?;
    let published = SOURCES.lock().publish(owner, identity, file, length);
    match published
    {
        Ok(source) => Ok((source, true)),
        Err(error) =>
        {
            files(|files| files.close_block_source(file))?;
            Err(error)
        }
    }
}
pub(crate) fn release(provider: u32, source: u32) -> Result<(), Status>
{
    let owner = crate::drivers::provider_module(provider)?;
    let backing = SOURCES.lock().begin_close(owner, source)?;
    match files(|files| files.close_block_source(backing))
    {
        Ok(()) => SOURCES.lock().finish_close(owner, source),
        Err(error) =>
        {
            SOURCES.lock().quarantine(owner, source)?;
            Err(error)
        }
    }
}
fn request(call: ResidentCall<'_>, output: &mut [u8]) -> Result<ResidentCompletion, Status>
{
    if call.phase != abi::START
    {
        return Err(BAD_ARGUMENT);
    }
    let operation = Operation::try_from(call.operation).map_err(|_| BAD_ARGUMENT)?;
    let args = Arguments::decode(operation, call.input).map_err(|_| BAD_ARGUMENT)?;
    match operation
    {
        Operation::Info =>
        {
            let info = SOURCES.lock().info(call.caller, args.source)?;
            output.get_mut(..wire::INFO_BYTES).ok_or(BAD_ARGUMENT)?.copy_from_slice(&info.encode());
            Ok(ResidentCompletion::Complete(wire::INFO_BYTES))
        }
        Operation::Read =>
        {
            let count = args.length as usize;
            if output.len() < count
            {
                return Err(BAD_ARGUMENT);
            }
            let plan =
                SOURCES.lock()
                       .begin_read(call.caller, args.source, call.ticket, args.offset, count)?;
            // No source, registry or Files guard survives entering the backend.
            let result = files(|files| {
                files.read_block_source(plan.backing, plan.offset, &mut output[..plan.length])
            });
            SOURCES.lock().finish_read(call.caller, args.source, call.ticket)?;
            result.map(ResidentCompletion::Complete)
        }
    }
}
