//! Platform-independent formats for build tools, loaders and future disk apps.
//! No filesystem, subprocesses, host dependencies, or operating-system calls.
#![no_std]

extern crate alloc;

pub mod elf;
pub mod macbinary;
pub mod native;
pub mod resource;
pub mod text;

use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub &'static str);
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        f.write_str(self.0)
    }
}
impl core::error::Error for Error
{
}
pub type Result<T> = core::result::Result<T, Error>;

pub(crate) fn slice(bytes: &[u8], offset: usize, length: usize) -> Result<&[u8]>
{
    let end = offset.checked_add(length).ok_or(Error("offset overflow"))?;
    bytes.get(offset..end).ok_or(Error("truncated data"))
}
pub(crate) fn be16(bytes: &[u8], offset: usize) -> Result<u16>
{
    Ok(u16::from_be_bytes(slice(bytes, offset, 2)?.try_into().unwrap()))
}
pub(crate) fn be32(bytes: &[u8], offset: usize) -> Result<u32>
{
    Ok(u32::from_be_bytes(slice(bytes, offset, 4)?.try_into().unwrap()))
}

pub mod code;
