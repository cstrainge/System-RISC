//! Native host-container format: one file, two forks and full native metadata.
//! No platform I/O. All allocation, offsets and chunk work are explicitly bounded.
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;
mod header;
mod stream;
mod record;
use alloc::string::String;

pub use header::{ForkLayout, Header, Layout};
pub use record::{decode_chunk, decode_metadata, encode_chunk, encode_metadata};
pub use stream::{Opened, Writer};
pub const VERSION: u32 = 1;
pub const MAGIC: &[u8; 8] = b"RSFORK01";
pub const HEADER_BYTES: usize = 256;
pub const METADATA_BYTES: usize = 1024;
pub const CHUNK_BYTES: usize = 4096;
pub const HASH_BYTES: u64 = 32;
pub const MAX_NAME: usize = 255;
pub const MAX_COMMENT: usize = 255;
pub const ROOT_RECORD: &str = ".system-risc-root";
pub const DIRECTORY_RECORD: &str = ".system-risc-directory";
pub const UNICODE_VERSION: u32 = mac_hfs_b::names::UNICODE_VERSION;
/// Portable user-visible host component. Reserve provider bookkeeping and the
/// Win32 device/character namespace even when the broker runs on Linux/WSL.
/// https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file
pub fn host_component(name: &str) -> Result<()>
{
    if name.is_empty()
       || name.len() > MAX_NAME
       || name.ends_with(['.', ' '])
       || name.bytes().any(|b| b < 32 || b"<>:\"/\\|?*".contains(&b))
       || name.get(..13).is_some_and(|s| s.eq_ignore_ascii_case(".system-risc-"))
    {
        return Err(Error::Invalid);
    }
    let stem = name.split('.').next().unwrap_or("").trim_end_matches(' ');
    if ["CON", "PRN", "AUX", "NUL"].iter().any(|s| stem.eq_ignore_ascii_case(s))
    {
        return Err(Error::Invalid);
    }
    if stem.get(..3).is_some_and(|s| s.eq_ignore_ascii_case("COM") || s.eq_ignore_ascii_case("LPT"))
       && stem.get(3..).is_some_and(|s| {
                           matches!(s,
                                    "1"
                                    | "2"
                                    | "3"
                                    | "4"
                                    | "5"
                                    | "6"
                                    | "7"
                                    | "8"
                                    | "9"
                                    | "¹"
                                    | "²"
                                    | "³")
                       })
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Invalid,
    Version,
    Corrupt,
    Range,
    Io,
    NoMemory,
    Stale,
    Incomplete
}
pub type Result<T> = core::result::Result<T, Error>;
impl core::fmt::Display for Error
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        write!(f, "{self:?}")
    }
}
impl core::error::Error for Error
{
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Kind
{
    File      = 1,
    Directory = 2,
    Root      = 3
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fork
{
    Data,
    Resource
}
/// IDs here are persistent catalog metadata, not live object handles. Runtime
/// mount/open/request lifetimes belong to the resident handle facility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metadata
{
    pub kind:            Kind,
    pub catalog_id:      u32,
    pub next_catalog_id: u32,
    pub identity:        [u8; 16],
    pub volume:          [u8; 16],
    pub revision:        u64,
    pub created:         u64,
    pub modified:        u64,
    pub backup:          Option<u64>,
    pub finder_info:     [u8; 32],
    pub attributes:      u16,
    pub name:            String,
    pub comment:         String
}
fn name_error(error: mac_hfs_b::Error) -> Error
{
    match error
    {
        mac_hfs_b::Error::NoMemory => Error::NoMemory,
        _ => Error::Invalid
    }
}
impl Metadata
{
    /// Fallible copying for bounded kernel/module heaps. Reserve both strings
    /// before any caller publishes metadata or acknowledges a host mutation.
    pub fn try_clone(&self) -> Result<Self>
    {
        fn text(source: &str) -> Result<String>
        {
            let mut value = String::new();
            value.try_reserve_exact(source.len()).map_err(|_| Error::NoMemory)?;
            value.push_str(source);
            Ok(value)
        }
        Ok(Self { name: text(&self.name)?,
                  comment: text(&self.comment)?,
                  ..*self })
    }

    pub fn new(kind: Kind, catalog_id: u32, volume: [u8; 16], name: &str, when: u64)
               -> Result<Self>
    {
        let result = Self { kind,
                            catalog_id,
                            next_catalog_id: if kind == Kind::Root { 16 } else { 0 },
                            identity: if kind == Kind::Root
                            {
                                volume
                            }
                            else
                            {
                                catalog_identity(volume, catalog_id)?
                            },
                            volume,
                            revision: 1,
                            created: when,
                            modified: when,
                            backup: None,
                            finder_info: [0; 32],
                            attributes: 0,
                            name: mac_hfs_b::names::normalize(name).map_err(name_error)?,
                            comment: String::new() };
        result.validate()?;
        Ok(result)
    }

    pub fn set_name(&mut self, name: &str) -> Result<()>
    {
        let name = mac_hfs_b::names::normalize(name).map_err(name_error)?;
        self.name = name;
        Ok(())
    }

    pub fn validate(&self) -> Result<()>
    {
        if self.identity == [0; 16]
           || self.volume == [0; 16]
           || self.revision == 0
           || self.comment.len() > MAX_COMMENT
           || mac_hfs_b::names::normalize(&self.name).map_err(name_error)? != self.name
        {
            return Err(Error::Invalid);
        }
        if self.kind == Kind::Root
        {
            if self.catalog_id != 2 || self.identity != self.volume || self.next_catalog_id < 16
            {
                return Err(Error::Invalid);
            }
        }
        else if self.catalog_id < 16
                  || self.next_catalog_id != 0
                  || self.identity != catalog_identity(self.volume, self.catalog_id)?
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
/// Export-scoped stable identity. This mapping is injective for all u32
/// catalog IDs and cannot collide with that export's root identity.
pub fn catalog_identity(volume: [u8; 16], id: u32) -> Result<[u8; 16]>
{
    if volume == [0; 16] || id < 16
    {
        return Err(Error::Invalid);
    }
    let mut identity = volume;
    let suffix = u32::from_le_bytes(volume[12..].try_into().unwrap()).wrapping_add(id);
    identity[12..].copy_from_slice(&suffix.to_le_bytes());
    if identity == [0; 16]
    {
        identity[0] = 0x80;
    }
    Ok(identity)
}

/// Adapters must expose a stable leased revision for a whole operation. Detect
/// in-place external edits or hold an immutable snapshot; don't mix revisions.
pub trait ReadAt
{
    fn len(&self) -> u64;
    /// Validate the leased revision before publishing bytes to a caller. The
    /// default suits immutable buffers or adapters which check every transfer.
    fn validate(&self) -> Result<()>
    {
        Ok(())
    }
    fn is_empty(&self) -> bool
    {
        self.len() == 0
    }
    fn read_exact(&mut self, offset: u64, output: &mut [u8]) -> Result<()>;
}
impl<T: ReadAt + ?Sized> ReadAt for &mut T
{
    fn len(&self) -> u64
    {
        (**self).len()
    }

    fn validate(&self) -> Result<()>
    {
        (**self).validate()
    }

    fn read_exact(&mut self, offset: u64, output: &mut [u8]) -> Result<()>
    {
        (**self).read_exact(offset, output)
    }
}
/// Write only to unpublished staging files. Caller owns atomic publication and
/// directory durability; a failed write/flush is never a committed file.
pub trait WriteAt: ReadAt
{
    fn set_len(&mut self, length: u64) -> Result<()>;
    fn write_all(&mut self, offset: u64, input: &[u8]) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
}
