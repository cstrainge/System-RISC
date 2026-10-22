//! Bounded StuffIt catalogs and resumable fork expansion, shared by native
//! filesystem drivers and Linux tools. Classic revisions 1/2 and StuffIt 5 are
//! selected by their headers. Catalog construction never reads fork payloads.
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;
mod crc;
mod header;
mod index;
mod builder;
mod v5;
pub use builder::IndexBuilder;
mod decode;
pub use crc::Crc16;
pub use decode::{Decoder, MAX_STEP, Progress};
pub use header::*;
pub use index::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Truncated,
    Signature,
    UnsupportedVersion(u8),
    UnsupportedEntryVersion(u8),
    HeaderChecksum,
    ForkChecksum,
    Name,
    Bounds,
    Hierarchy,
    Count,
    Limit,
    Allocation,
    CompressedData,
    Length,
    InvalidState,
    Encrypted,
    UnsupportedMethod(u8)
}
pub type Result<T> = core::result::Result<T, Error>;
#[cfg(test)]
mod tests;
