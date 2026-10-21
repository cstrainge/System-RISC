//! Read-only archive volumes. System supplies all live tokens and owns source
//! leases. This module owns immutable catalogs and bounded streaming decoders.
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;
mod catalog;
mod stream;
pub mod service;
use alloc::vec::Vec;

use mac_driver::{Completion, Error, Phase, Result, file_sources::FileSources};

/// Testable capability boundary; native code uses only resident FORK services.
pub trait Sources: Send
{
    fn length(&self, source: u32) -> Result<u64>;
    fn read(&mut self, source: u32, offset: u64, ticket: &mut u32, phase: Phase, output: &mut [u8])
            -> Result<Completion>;
}
impl Sources for FileSources
{
    fn length(&self, source: u32) -> Result<u64>
    {
        Ok(self.info(source)?.length)
    }

    fn read(&mut self, source: u32, offset: u64, ticket: &mut u32, phase: Phase, output: &mut [u8])
            -> Result<Completion>
    {
        FileSources::read(*self, source, offset, ticket, phase, output)
    }
}
fn buffer(length: usize) -> Result<Vec<u8>>
{
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length).map_err(|_| Error(mac_abi::NO_MEMORY))?;
    bytes.resize(length, 0);
    Ok(bytes)
}
fn format_error(error: mac_sit::Error) -> Error
{
    Error(match error
          {
              mac_sit::Error::Allocation => mac_abi::NO_MEMORY,
              mac_sit::Error::Encrypted
              | mac_sit::Error::UnsupportedMethod(_)
              | mac_sit::Error::UnsupportedVersion(_) => mac_abi::NOT_SUPPORTED,
              mac_sit::Error::Limit => mac_abi::NO_MEMORY,
              _ => mac_abi::IO_ERROR
          })
}
