//! Read-only mapping inspection. Source data is owned by the actual address
//! space; no resident snapshot handle or second mapping registry exists.
#![forbid(unsafe_code)]
use core::fmt::{self, Write};

use mac_abi::{inspection::{INSPECT_DOMAINS, memory::*},
              *};

pub(crate) trait Source
{
    type Error: fmt::Display;
    fn status(error: &Self::Error) -> Status;
    fn revision(&self) -> Result<u64, Self::Error>;
    fn summary(&self) -> Result<Summary, Self::Error>;
    fn regions(&self) -> Result<&[Region], Self::Error>;
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

#[cfg(test)]
mod tests
{
    extern crate std;
    use core::cell::Cell;
    use std::vec::Vec;

    use super::*;
    struct Fixture
    {
        revision: Cell<u64>,
        rows:     Vec<Region>,
        fail:     Cell<bool>
    }
    impl Source for Fixture
    {
        type Error = &'static str;

        fn status(_: &Self::Error) -> Status
        {
            NO_MEMORY
        }

        fn revision(&self) -> Result<u64, Self::Error>
        {
            Ok(self.revision.get())
        }

        fn summary(&self) -> Result<Summary, Self::Error>
        {
            self.regions()?;
            Ok(Summary { revision: self.revision.get(),
                         region_count: self.rows.len() as u64,
                         ..Default::default() })
        }

        fn regions(&self) -> Result<&[Region], Self::Error>
        {
            if self.fail.get()
            {
                Err("mapping inventory allocation refused; release memory and retry")
            }
            else
            {
                Ok(&self.rows)
            }
        }
    }
    fn source() -> Fixture
    {
        Fixture { revision: Cell::new(1),
                  fail:     Cell::new(false),
                  rows:     (0..39).map(|i| {
                                       Region { virtual_base:  i * 8192,
                                                physical_base: i * 4096,
                                                bytes:         4096,
                                                protection:    READABLE,
                                                kind:          OTHER }
                                   })
                                   .collect() }
    }
    fn detail(call: &Call) -> &str
    {
        core::str::from_utf8(&call.detail[..call.detail_length as usize]).unwrap()
    }
    #[test]
    fn batches_are_revisioned_unbounded_and_reauthenticate_every_read()
    {
        let source = source();
        let mut call = Call::default();
        assert_eq!(request(Some(INSPECT_DOMAINS), &source, &mut call), OK);
        assert_eq!((call.revision, call.summary.region_count), (1, 39));
        call.operation = READ;
        for (start, count, flags) in [(0, 16, HAS_NEXT), (16, 16, HAS_NEXT), (32, 7, 0)]
        {
            call.index = start;
            assert_eq!(request(Some(INSPECT_DOMAINS), &source, &mut call), OK);
            assert_eq!((call.count, call.flags, call.next),
                       (count, flags, start + u64::from(count)));
            assert_eq!(call.regions[0], source.rows[start as usize]);
        }
        assert_eq!(request(Some(0), &source, &mut call), ACCESS_DENIED);
        assert!(detail(&call).contains("inspection policy grant"));
        assert_eq!(call.count, 0);
        assert!(call.regions.iter().all(|r| *r == Region::default()),
                "no previous reply leaks through refusal");
        assert_eq!(request(None, &source, &mut call), ACCESS_DENIED);
        assert!(detail(&call).contains("live process domain"));
        source.revision.set(2);
        assert_eq!(request(Some(INSPECT_DOMAINS), &source, &mut call), SCAN_STALE);
        assert!(detail(&call).contains("obtain a new snapshot and retry"));
    }
    #[test]
    fn exhaustion_is_informative_retryable_and_bad_positions_do_not_damage_source()
    {
        let source = source();
        let mut call = Call::default();
        source.fail.set(true);
        assert_eq!(request(Some(INSPECT_DOMAINS), &source, &mut call), NO_MEMORY);
        assert!(detail(&call).contains("release memory and retry"));
        source.fail.set(false);
        assert_eq!(request(Some(INSPECT_DOMAINS), &source, &mut call), OK);
        call.operation = READ;
        call.index = u64::MAX;
        assert_eq!(request(Some(INSPECT_DOMAINS), &source, &mut call), NOT_FOUND);
        assert!(detail(&call).contains("outside this System mapping snapshot"));
        call.index = 0;
        call.reserved = 1;
        assert_eq!(request(Some(INSPECT_DOMAINS), &source, &mut call), BAD_ARGUMENT);
        assert!(detail(&call).contains("reserved field"));
        call.reserved = 0;
        assert_eq!(request(Some(INSPECT_DOMAINS), &source, &mut call), OK);
        assert_eq!(call.regions[0], source.rows[0]);
    }
}
