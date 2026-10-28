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
    fn length(&self, source: u64) -> Result<u64>;
    fn read(&mut self, source: u64, offset: u64, ticket: &mut u64, phase: Phase, output: &mut [u8])
            -> Result<Completion>;
}
impl Sources for FileSources
{
    fn length(&self, source: u64) -> Result<u64>
    {
        Ok(self.info(source)?.length)
    }

    fn read(&mut self, source: u64, offset: u64, ticket: &mut u64, phase: Phase, output: &mut [u8])
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
    use mac_abi::archive::ArchiveError;
    Error(match error
          {
              mac_sit::Error::Allocation => mac_abi::NO_MEMORY,
              mac_sit::Error::Encrypted => ArchiveError::Encrypted.status(),
              mac_sit::Error::UnsupportedMethod(method) => ArchiveError::Method(method).status(),
              mac_sit::Error::UnsupportedVersion(version) =>
              {
                  ArchiveError::Version(version).status()
              }
              mac_sit::Error::UnsupportedEntryVersion(version) =>
              {
                  ArchiveError::EntryVersion(version).status()
              }
              mac_sit::Error::Limit => ArchiveError::Limit.status(),
              mac_sit::Error::Truncated => ArchiveError::Truncated.status(),
              mac_sit::Error::Signature => ArchiveError::Signature.status(),
              mac_sit::Error::HeaderChecksum => ArchiveError::HeaderChecksum.status(),
              mac_sit::Error::ForkChecksum => ArchiveError::ForkChecksum.status(),
              _ => ArchiveError::Malformed.status()
          })
}

/// Synchronous retained-state construction only; request buffers and lower
/// source invocations keep their original admitted allocation target.
fn module_allocation<T>(operation: impl FnOnce() -> mac_driver::Result<T>)
                        -> mac_driver::Result<T>
{
    mac_driver::runtime::allocation::with_module_allocation(operation)?
}
fn module_reserve<T>(values: &mut alloc::vec::Vec<T>, additional: usize) -> mac_driver::Result<()>
{
    module_allocation(|| {
        values.try_reserve(additional).map_err(|_| mac_driver::Error(mac_abi::NO_MEMORY))
    })
}
