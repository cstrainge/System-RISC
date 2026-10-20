//! HFS-B codecs and filesystem algorithms shared by ROM, System and host tools.
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;

pub mod names;
pub mod time;
pub mod layout;
mod tree;
mod volume;
pub use mac_block::{BlockDevice, Partition, WritableBlockDevice};
pub use volume::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Invalid,
    Incompatible,
    Corrupt,
    Dirty,
    NotFound,
    Exists,
    Protected,
    ReadOnly,
    Full,
    Budget,
    NoMemory,
    Io,
    RollbackFailed
}
pub type Result<T> = core::result::Result<T, Error>;
impl core::fmt::Display for Error
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        f.write_str(match self
                    {
                        Self::Invalid => "invalid HFS-B request",
                        Self::Incompatible => "incompatible HFS-B format; recreate volume",
                        Self::Corrupt => "malformed HFS-B metadata",
                        Self::Dirty => "unclean HFS-B volume; check before mounting",
                        Self::NotFound => "HFS-B object not found",
                        Self::Exists => "HFS-B name already exists",
                        Self::Protected => "HFS-B object is protected",
                        Self::ReadOnly => "HFS-B device is read-only",
                        Self::Full => "HFS-B volume is full",
                        Self::Budget => "HFS-B operation exceeds supplied work budget",
                        Self::NoMemory => "HFS-B allocation failed",
                        Self::Io => "HFS-B storage operation failed",
                        Self::RollbackFailed => "HFS-B rollback failed; volume requires check"
                    })
    }
}
impl core::error::Error for Error
{
}
impl From<mac_block::Error> for Error
{
    fn from(_: mac_block::Error) -> Self
    {
        Self::Io
    }
}
pub(crate) fn copy(bytes: &[u8]) -> Result<alloc::vec::Vec<u8>>
{
    let mut out = alloc::vec::Vec::new();
    out.try_reserve_exact(bytes.len()).map_err(|_| Error::NoMemory)?;
    out.extend_from_slice(bytes);
    Ok(out)
}
