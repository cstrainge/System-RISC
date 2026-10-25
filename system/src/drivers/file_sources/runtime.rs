//! Resident backing-file service. Block reads complete directly; provider reads
//! retain owned jobs and kernel fork marks until actual acknowledgement.
#![forbid(unsafe_code)]
use mac_file_source_interface::{self as wire, Arguments, Operation};
use mac_module::{Offer, abi};
use spinning_top::Spinlock;

use super::*;
use crate::drivers::{ResidentCall, ResidentCompletion};
mod reads;
pub(crate) struct Closing
{
    owner:   u64,
    source:  u32,
    backing: u32
}
pub(crate) fn begin_release(provider: u32, source: u32)
                            -> Result<Option<(Closing, crate::drivers::OwnedJob)>, Status>
{
    let owner = crate::drivers::provider_module(provider)?;
    let backing = SOURCES.lock().begin_close(owner, source)?;
    let close = Closing { owner,
                          source,
                          backing };
    let result =
        files(|manager| manager.source_mark_provider(backing)).and_then(|provider| {
            if provider.is_none()
            {
                files(|manager| manager.close_block_source(backing))?;
                return Ok(None);
            }
            let (provider, args) = files(|manager| manager.close_provider_source(backing, false))?;
            reads::submit(provider, mac_filesystem_interface::Operation::Close, args, 0).map(Some)
        });
    match result
    {
        Ok(Some(job)) => Ok(Some((close, job))),
        Ok(None) =>
        {
            SOURCES.lock().finish_close(owner, source)?;
            Ok(None)
        }
        Err(error) =>
        {
            let _ = files(|m| m.quarantine_provider_source(backing));
            SOURCES.lock().quarantine(owner, source)?;
            Err(error)
        }
    }
}
pub(crate) fn finish_release(close: Closing, result: Result<alloc::vec::Vec<u8>, Status>)
                             -> Result<(), Status>
{
    if matches!(&result, Ok(bytes) if bytes.is_empty()) || matches!(result, Err(mac_abi::NOT_FOUND))
    {
        if let Err(error) = files(|m| m.retire_provider_source(close.backing))
        {
            SOURCES.lock().quarantine(close.owner, close.source)?;
            return Err(error);
        }
        return SOURCES.lock().finish_close(close.owner, close.source);
    }
    let _ = files(|m| m.quarantine_provider_source(close.backing));
    SOURCES.lock().quarantine(close.owner, close.source)?;
    Err(result.err().unwrap_or(mac_abi::BAD_ABI))
}
static SOURCES: Spinlock<Sources<u32>> = Spinlock::new(Sources::new());
pub(crate) fn files<T>(call: impl FnOnce(&mut crate::toolbox::files::Manager) -> Result<T, Status>)
                       -> Result<T, Status>
{
    crate::kernel::with_files(call).map_err(crate::state::AccessError::status)?
}
pub(crate) fn find(provider: u32, volume: u32, cnid: u32) -> Result<Option<u32>, Status>
{
    let owner = crate::drivers::provider_module(provider)?;
    SOURCES.lock().find(owner,
                        Identity { volume,
                                   cnid,
                                   fork: mac_abi::files::DATA_FORK })
}
pub(crate) fn adopt(provider: u32, volume: u32, cnid: u32, handle: u32, length: u64)
                    -> Result<u32, Status>
{
    let owner = crate::drivers::provider_module(provider)?;
    SOURCES.lock().publish(owner,
                           Identity { volume,
                                      cnid,
                                      fork: mac_abi::files::DATA_FORK },
                           handle,
                           length)
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
    match operation
    {
        Operation::Info =>
        {
            let info = SOURCES.lock().info(call.caller, args.source)?;
            output.get_mut(..wire::INFO_BYTES).ok_or(BAD_ARGUMENT)?.copy_from_slice(&info.encode());
            Ok(ResidentCompletion::Complete(wire::INFO_BYTES))
        }
        Operation::Read => Err(BAD_ARGUMENT)
    }
}
