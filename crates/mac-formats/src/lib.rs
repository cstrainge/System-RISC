//! Platform-independent formats for build tools, loaders and future disk apps.
//! No filesystem, subprocesses, host dependencies, or operating-system calls.
#![no_std]

extern crate alloc;

pub mod elf;
pub mod macbinary;
pub mod native;
pub mod resource;
pub mod text;

pub use mac_code_resource::{Error, Result};

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
pub use mac_code_resource::code_types;
pub mod relocation;
