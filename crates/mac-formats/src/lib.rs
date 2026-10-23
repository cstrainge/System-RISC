//! Platform-independent formats for build tools, loaders and future disk apps.
//! No filesystem, subprocesses, host dependencies, or operating-system calls.
#![no_std]

extern crate alloc;

pub mod elf;
pub mod authority;
pub mod macbinary;
pub mod native;
pub mod module;
pub mod open_types;
pub mod resource;
pub mod text;

/// Semantic error identity shared by OS loaders and host tools. Diagnostic
/// prose is deliberately independent of the category used at service boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error
{
    BudgetExceeded(&'static str),
    AllocationFailed(&'static str),
    Missing(&'static str),
    TranslationRequired(&'static str),
    Malformed(&'static str)
}
impl Error
{
    pub const fn message(self) -> &'static str
    {
        match self
        {
            Self::BudgetExceeded(message)
            | Self::AllocationFailed(message)
            | Self::Missing(message)
            | Self::TranslationRequired(message)
            | Self::Malformed(message) => message
        }
    }
}
impl core::fmt::Display for Error
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        f.write_str(self.message())
    }
}
impl core::error::Error for Error
{
}
impl From<mac_code_resource::Error> for Error
{
    fn from(error: mac_code_resource::Error) -> Self
    {
        // The allocation-free descriptor/segment codec reports only malformed
        // wire data. It has no allocation, admission budget or resource lookup.
        // Preserve its detail verbatim; never infer a cause from that detail.
        Self::Malformed(error.0)
    }
}
pub type Result<T> = core::result::Result<T, Error>;

pub(crate) fn slice(bytes: &[u8], offset: usize, length: usize) -> Result<&[u8]>
{
    let end = offset.checked_add(length).ok_or(Error::Malformed("offset overflow"))?;
    bytes.get(offset..end).ok_or(Error::Malformed("truncated data"))
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
