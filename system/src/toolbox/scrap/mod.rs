//! Resident shared, memory-backed Scrap Manager. The kernel's TryState serializes
//! all callers; no caller pointers or borrowed payloads survive a request.
#![deny(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::{scrap::*, *};

// The sole pointer boundary is isolated in this module.
#[allow(unsafe_code)]
pub mod native;
struct Record
{
    kind:   [u8; 4],
    bytes:  Vec<u8>,
    offset: u32
}
#[derive(Default)]
pub struct Manager
{
    records: Vec<Record>,
    size:    u32,
    count:   u16
}
impl Manager
{
    /// Replace the shared clipboard in one transaction for native editor Copy.
    /// Allocation and record construction finish before the old generation is
    /// released, so an error preserves the previous clipboard in full.
    pub fn replace_text(&mut self, text: &str) -> Result<(), Status>
    {
        let mut replacement = Self::default();
        replacement.put(TEXT, text.as_bytes())?;
        replacement.count = self.count.wrapping_add(1);
        *self = replacement;
        Ok(())
    }

    /// Borrow the first native text record while holding the Scrap guard.
    pub fn text(&self) -> Result<&str, Status>
    {
        let record = self.records.iter().find(|record| record.kind == TEXT).ok_or(NO_TYPE_ERR)?;
        core::str::from_utf8(&record.bytes).map_err(|_| BAD_ARGUMENT)
    }

    pub fn request(&mut self, call: &mut ScrapCall, input: &[u8], output: &mut [u8]) -> Status
    {
        call.transferred = 0;
        let result =
            match call.operation
            {
                INFO => Ok(()),
                CLEAR =>
                {
                    self.records.clear();
                    self.size = 0;
                    self.count = self.count.wrapping_add(1);
                    Ok(())
                }
                PUT =>
                {
                    if call.kind == TEXT { Err(BAD_ARGUMENT) } else { self.put(call.kind, input) }
                }
                PUT_TEXT =>
                {
                    match core::str::from_utf8(input)
                    {
                        Ok(_) => self.put(TEXT, input),
                        Err(_) => Err(BAD_ARGUMENT)
                    }
                }
                FLAVOR_AT =>
                {
                    match self.records.get(call.index as usize)
                    {
                        Some(record) =>
                        {
                            call.flavor = record.info();
                            Ok(())
                        }
                        None => Err(NOT_FOUND)
                    }
                }
                READ | READ_TEXT => self.read(call, output),
                LOAD | UNLOAD => Err(NOT_SUPPORTED),
                _ => Err(NOT_SUPPORTED)
            };
        call.info = ScrapInfo { size:    self.size,
                                count:   self.count,
                                flavors: self.records.len() as u16 };
        result.map_or_else(|error| error, |_| OK)
    }

    fn put(&mut self, kind: [u8; 4], input: &[u8]) -> Result<(), Status>
    {
        let size = self.size as usize + 8 + input.len() + (input.len() & 1);
        if size > MAX_BYTES || self.records.len() >= MAX_FLAVORS
        {
            return Err(NO_MEMORY);
        }
        // Reserve and copy into staging before touching logical scrap state.
        // Fallible reservation never loses existing records or increments count.
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(input.len()).map_err(|_| NO_MEMORY)?;
        bytes.extend_from_slice(input);
        self.records.try_reserve(1).map_err(|_| NO_MEMORY)?;
        self.records.push(Record { kind,
                                   bytes,
                                   offset: self.size + 8 });
        self.size = size as u32;
        Ok(())
    }

    fn read(&self, call: &mut ScrapCall, output: &mut [u8]) -> Result<(), Status>
    {
        let text = call.operation == READ_TEXT;
        if !text && call.kind == TEXT || text && call.offset != 0
        {
            return Err(BAD_ARGUMENT);
        }
        let kind = if text { TEXT } else { call.kind };
        let record = self.records.iter().find(|record| record.kind == kind).ok_or(NO_TYPE_ERR)?;
        call.flavor = record.info();
        if text
        {
            if output.len() < record.bytes.len()
            {
                return Err(NO_MEMORY);
            }
            output[..record.bytes.len()].copy_from_slice(&record.bytes);
            call.transferred = record.bytes.len();
        }
        else
        {
            let input = record.bytes.get(call.offset as usize..).ok_or(BAD_ARGUMENT)?;
            let length = input.len().min(output.len());
            output[..length].copy_from_slice(&input[..length]);
            call.transferred = length;
        }
        Ok(())
    }
}
impl Record
{
    fn info(&self) -> FlavorInfo
    {
        FlavorInfo { kind:   self.kind,
                     length: self.bytes.len() as u32,
                     offset: self.offset }
    }
}
