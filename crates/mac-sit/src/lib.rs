//! Bounded classic StuffIt 1.5.1 parsing, shared by kernel drivers and tools.
//! The resumable catalog builder asks for headers only. It never buffers fork
//! payloads, interprets a folder's subtree hints as forks, or probes SIT5/SITX.
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;
mod crc;
mod header;
mod index;
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
