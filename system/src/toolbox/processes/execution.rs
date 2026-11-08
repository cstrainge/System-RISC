//! Execute participants ask the broker for evidence about the target file.
use alloc::vec::Vec;

use mac_abi::{BAD_ABI, BAD_ARGUMENT, NO_MEMORY, NOT_SUPPORTED, OK, OPERATION_CANCELLED,
              SERVICE_BUSY, Status, processes::execution as wire};

use crate::{drivers, toolbox::files::application};

pub(in crate::toolbox) fn authorize(owner: u64, volume: u32, cnid: u32, streams: [u64; 3])
                                    -> Result<(), Status>
{
    let handlers = drivers::execution_handlers()?;
    if handlers.is_empty()
    {
        return default_profile(streams);
    }
    let caller = crate::context::current_owner()?;
    if caller.owner().incarnation() != owner
    {
        return Err(mac_abi::ACCESS_DENIED);
    }
    let subject = drivers::jobs::subjects::resident()?.admit_current(
        &crate::toolbox::handles::subject::Authority::Client(caller))?;
    let mut identity = [0; wire::IDENTITY_BYTES];
    identity[..4].copy_from_slice(&volume.to_le_bytes());
    identity[4..].copy_from_slice(&cnid.to_le_bytes());
    let mut catalog_reply = None;
    for provider in handlers
    {
        let mut decision = inspect(&subject, provider, wire::INSPECT, &identity)?;
        for query in 0..=wire::MAX_QUERIES
        {
            decision = match decision.as_slice()
            {
                [wire::PASS] => break,
                [wire::CONSOLE] =>
                {
                    if streams.contains(&0)
                    {
                        return Err(BAD_ARGUMENT);
                    }
                    let mut attach = [0; wire::ATTACH_BYTES];
                    attach[..wire::IDENTITY_BYTES].copy_from_slice(&identity);
                    for (chunk, handle) in
                        attach[wire::IDENTITY_BYTES..].chunks_exact_mut(8).zip(streams)
                    {
                        chunk.copy_from_slice(&handle.to_le_bytes());
                    }
                    let reply = inspect(&subject, provider, wire::ATTACH, &attach)?;
                    return if reply == attach[wire::IDENTITY_BYTES..]
                    {
                        Ok(())
                    }
                    else
                    {
                        Err(BAD_ABI)
                    };
                }
                _ if query == wire::MAX_QUERIES => return Err(BAD_ABI),
                [wire::QUERY_CATALOG] =>
                {
                    if catalog_reply.is_none()
                    {
                        let catalog = application::catalog(owner, volume, cnid)?;
                        let mut reply = [0; wire::CATALOG_BYTES];
                        reply[..wire::IDENTITY_BYTES].copy_from_slice(&identity);
                        reply[wire::IDENTITY_BYTES..wire::IDENTITY_BYTES + 4]
                            .copy_from_slice(&catalog.file_type);
                        reply[wire::IDENTITY_BYTES + 4..].copy_from_slice(&catalog.creator);
                        catalog_reply = Some(reply);
                    }
                    inspect(&subject,
                            provider,
                            wire::CATALOG_RESULT,
                            catalog_reply.as_ref().ok_or(BAD_ABI)?)?
                }
                [wire::QUERY_DATA | wire::QUERY_RESOURCE, ..] =>
                {
                    let reply = fork_reply(owner, volume, cnid, &identity, &decision)?;
                    inspect(&subject, provider, wire::FORK_RESULT, &reply)?
                }
                _ => return Err(BAD_ABI)
            };
        }
    }
    default_profile(streams)
}

fn default_profile(streams: [u64; 3]) -> Result<(), Status>
{
    if streams == [0; 3] { Ok(()) } else { Err(NOT_SUPPORTED) }
}

fn fork_reply(owner: u64, volume: u32, cnid: u32, identity: &[u8; wire::IDENTITY_BYTES],
              query: &[u8])
              -> Result<Vec<u8>, Status>
{
    if query.len() != wire::FORK_QUERY_BYTES
    {
        return Err(BAD_ABI);
    }
    let offset = u64::from_le_bytes(query[1..9].try_into().map_err(|_| BAD_ABI)?);
    let length = u16::from_le_bytes(query[9..].try_into().map_err(|_| BAD_ABI)?) as usize;
    if length == 0 || length > wire::MAX_FORK_BYTES || offset.checked_add(length as u64).is_none()
    {
        return Err(BAD_ARGUMENT);
    }
    let mut reply = Vec::new();
    reply.try_reserve_exact(wire::FORK_HEADER_BYTES + length).map_err(|_| NO_MEMORY)?;
    reply.extend_from_slice(identity);
    reply.extend_from_slice(query);
    let status_at = reply.len();
    reply.resize(wire::FORK_HEADER_BYTES + length, 0);
    let (status, read) = match application::probe_fork(owner,
                                                       volume,
                                                       cnid,
                                                       query[0] == wire::QUERY_RESOURCE,
                                                       offset,
                                                       &mut reply[wire::FORK_HEADER_BYTES..])
    {
        Ok(read) => (OK, read),
        Err(status) => (status, 0)
    };
    reply[status_at..wire::FORK_HEADER_BYTES].copy_from_slice(&status.to_le_bytes());
    reply.truncate(wire::FORK_HEADER_BYTES + read);
    Ok(reply)
}

fn inspect(subject: &drivers::jobs::subjects::Subject, provider: u64, operation: u32, bytes: &[u8])
           -> Result<Vec<u8>, Status>
{
    let mut input = Vec::new();
    input.try_reserve_exact(bytes.len()).map_err(|_| NO_MEMORY)?;
    input.extend_from_slice(bytes);
    let mut job = drivers::submit_background_to(subject,
                                                provider,
                                                mac_module::ClassId(wire::CLASS),
                                                wire::VERSION,
                                                operation,
                                                input,
                                                if operation == wire::ATTACH
                                                {
                                                    wire::STREAM_BYTES
                                                }
                                                else
                                                {
                                                    wire::FORK_QUERY_BYTES
                                                })?;
    job.run_once()?.ok_or(SERVICE_BUSY)?.result.map_err(|error| {
                                                   match error
                                                   {
                                                       drivers::jobs::Failure::Service(status) =>
                                                       {
                                                           status
                                                       }
                                                       drivers::jobs::Failure::Cancelled =>
                                                       {
                                                           OPERATION_CANCELLED
                                                       }
                                                   }
                                               })
}
