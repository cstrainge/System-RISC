//! Read-only mapping inspection. Source data is owned by the actual address
//! space; no resident snapshot handle or second mapping registry exists.
#![forbid(unsafe_code)]
use core::fmt::{self, Write};

use mac_abi::{inspection::{INSPECT_DOMAINS, memory::*},
              *};

pub(crate) trait Source
{
    type Error: fmt::Display;
    type Rows: AsRef<[Region]>;
    fn status(error: &Self::Error) -> Status;
    fn revision(&self) -> Result<u64, Self::Error>;
    fn summary(&self) -> Result<Summary, Self::Error>;
    fn regions(&self) -> Result<Self::Rows, Self::Error>;
}
struct Diagnostic<'a>
{
    bytes:  &'a mut [u8],
    length: usize
}
impl Write for Diagnostic<'_>
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        let length = text.len().min(self.bytes.len() - self.length);
        let mut end = length;
        while !text.is_char_boundary(end)
        {
            end -= 1;
        }
        self.bytes[self.length..self.length + end].copy_from_slice(&text.as_bytes()[..end]);
        self.length += end;
        if end < text.len() { Err(fmt::Error) } else { Ok(()) }
    }
}
fn reject(call: &mut Call, status: Status, message: impl fmt::Display) -> Status
{
    let mut output = Diagnostic { bytes:  &mut call.detail,
                                  length: 0 };
    let _ = write!(output, "{message}");
    call.detail_length = output.length as u32;
    status
}
/// Authority comes from the resident process entry, never the payload. System
/// mapping addresses are cross-domain detail; registry existence stays public.
pub(crate) fn request<S: Source>(authority: Option<u64>, source: &S, call: &mut Call) -> Status
{
    call.detail.fill(0);
    call.detail_length = 0;
    call.count = 0;
    call.next = 0;
    call.flags = 0;
    call.summary = Summary::default();
    call.regions.fill(Region::default());
    let Some(authority) = authority
    else
    {
        return reject(call, ACCESS_DENIED, "memory inspection caller has no live process domain");
    };
    if authority & INSPECT_DOMAINS == 0
    {
        return reject(call,
                      ACCESS_DENIED,
                      "System mapping details require a System inspection policy grant; public \
                       registry queries remain available");
    }
    if call.reserved != 0 || call.source != SYSTEM || ![BEGIN, READ].contains(&call.operation)
    {
        return reject(call, BAD_ARGUMENT,
                      "invalid memory inspection source, operation or reserved field");
    }
    if call.operation == BEGIN
    {
        return match source.summary()
        {
            Ok(summary) =>
            {
                call.revision = summary.revision;
                call.summary = summary;
                OK
            }
            Err(error) => reject(call, S::status(&error), error)
        };
    }
    let revision = match source.revision()
    {
        Ok(revision) => revision,
        Err(error) => return reject(call, S::status(&error), error)
    };
    if revision != call.revision
    {
        return reject(call, SCAN_STALE,
                      "System mapping snapshot changed; obtain a new snapshot and retry");
    }
    let rows = match source.regions()
    {
        Ok(rows) => rows,
        Err(error) => return reject(call, S::status(&error), error)
    };
    let rows = rows.as_ref();
    match source.revision()
    {
        Ok(revision) if revision == call.revision =>
        {}
        Ok(_) =>
        {
            return reject(call, SCAN_STALE,
                          "System mapping changed while reading; obtain a new snapshot and retry");
        }
        Err(cause) => return reject(call, S::status(&cause), cause)
    }
    let Some(index) = usize::try_from(call.index).ok().filter(|&index| index < rows.len())
    else
    {
        return reject(call, NOT_FOUND, "region index is outside this System mapping snapshot");
    };
    let count = (rows.len() - index).min(BATCH);
    call.regions[..count].copy_from_slice(&rows[index..index + count]);
    call.count = count as u32;
    call.next = (index + count) as u64;
    if index + count < rows.len()
    {
        call.flags = HAS_NEXT;
    }
    OK
}

