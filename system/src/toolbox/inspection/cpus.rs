//! Public CPU metadata, independent of inspector grants or policy installation.
#![forbid(unsafe_code)]
use mac_abi::{inspection::cpus::*, *};
fn reject(call: &mut Call, status: Status, text: &str) -> Status
{
    let mut length = text.len().min(call.detail.len());
    while !text.is_char_boundary(length)
    {
        length -= 1;
    }
    call.detail[..length].copy_from_slice(&text.as_bytes()[..length]);
    call.detail_length = length as u32;
    status
}
pub(crate) fn request(caller_live: bool, call: &mut Call) -> Status
{
    call.detail_length = 0;
    call.summary = Summary::default();
    call.row = Row::default();
    if !caller_live
    {
        return reject(call, ACCESS_DENIED, "CPU enumeration requires a live application context");
    }
    if call.reserved != 0 || call.reserved_tail != 0 || !matches!(call.operation, BEGIN | READ)
    {
        return reject(call, BAD_ARGUMENT, "invalid CPU inspection operation or reserved field");
    }
    if call.operation == BEGIN && call.index != 0
    {
        return reject(call, BAD_ARGUMENT, "CPU enumeration begin requires index zero");
    }
    let summary = match crate::machine::traps::cpus::summary()
    {
        Some(value) => value,
        None =>
        {
            return reject(call, SERVICE_BUSY, "CPU topology or admitted clock is not available");
        }
    };
    call.summary = summary;
    if call.operation == BEGIN
    {
        return OK;
    }
    let Ok(index) = usize::try_from(call.index)
    else
    {
        return reject(call, NOT_FOUND, "CPU index is outside the immutable topology");
    };
    match crate::machine::traps::cpus::row(index)
    {
        Ok(value) =>
        {
            call.row = value;
            OK
        }
        Err(crate::machine::traps::cpus::ReadError::Index) =>
        {
            reject(call, NOT_FOUND, "CPU index is outside the immutable topology")
        }
        Err(crate::machine::traps::cpus::ReadError::Binding) =>
        {
            reject(call, CORRUPT_DATA, "CPU topology, registry or monitor binding is unavailable")
        }
    }
}
