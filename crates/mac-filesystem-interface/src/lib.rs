//! Bounded, CPU-independent filesystem provider contract. This class consumes
//! directory exports or leased files as well as block devices; it has no sector
//! assumption. The resident File Manager issues all live object tokens and owns
//! application access checks. Providers own persistent catalog identities.
#![no_std]
#![forbid(unsafe_code)]
mod metadata;
pub use metadata::{METADATA_BYTES, MOUNT_BYTES, Metadata, Mount};
pub const CLASS: [u8; 4] = *b"FSYS";
pub const VERSION: u32 = 1;
pub const SCHEMA: u16 = 1;
pub const HEADER_BYTES: usize = 64;
pub const MAX_TRANSFER: usize = 64 * 1024;
pub const MAX_REQUEST: usize = HEADER_BYTES + MAX_TRANSFER;
pub const MAX_REPLY: usize = MAX_TRANSFER;
pub const DATA_FORK: u32 = 0;
pub const RESOURCE_FORK: u32 = 1;
/// Scan replies begin with one of these LE u32 values, followed by Metadata for
/// ENTRY only. An unscanned child's count uses an explicit unknown sentinel.
pub const SCAN_PENDING: u32 = 0;
pub const SCAN_ENTRY: u32 = 1;
pub const SCAN_DONE: u32 = 2;
pub const CHILD_COUNT_UNKNOWN: u32 = u32::MAX;
/// Provider source capabilities, used by class-neutral interface discovery.
pub const SOURCE_HOST_EXPORT: u64 = 1;
pub const SOURCE_FILE: u64 = 2;
/// Mounted volume capabilities. Writability is per mount, never inferred from
/// the provider's source type or filename. Unsupported mutations must fail.
pub const READ: u32 = 1;
pub const WRITE: u32 = 2;
pub const ALL_CAPABILITIES: u32 = READ | WRITE;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Version,
    Length,
    Invalid,
    Utf8
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Operation
{
    Exports       = 1,
    Mount         = 2,
    Unmount       = 3,
    Catalog       = 4,
    Scan          = 5,
    Lookup        = 6,
    Open          = 7,
    Read          = 8,
    Close         = 9,
    BeginCreate   = 10,
    BeginReplace  = 11,
    WriteChunk    = 12,
    FinishWrite   = 13,
    AbortWrite    = 14,
    Refresh       = 15,
    Rename        = 16,
    Move          = 17,
    MakeDirectory = 18,
    Remove        = 19,
    SetMetadata   = 20
}
impl TryFrom<u32> for Operation
{
    type Error = Error;

    fn try_from(value: u32) -> Result<Self, Error>
    {
        Ok(match value
        {
            1 => Self::Exports,
            2 => Self::Mount,
            3 => Self::Unmount,
            4 => Self::Catalog,
            5 => Self::Scan,
            6 => Self::Lookup,
            7 => Self::Open,
            8 => Self::Read,
            9 => Self::Close,
            10 => Self::BeginCreate,
            11 => Self::BeginReplace,
            12 => Self::WriteChunk,
            13 => Self::FinishWrite,
            14 => Self::AbortWrite,
            15 => Self::Refresh,
            16 => Self::Rename,
            17 => Self::Move,
            18 => Self::MakeDirectory,
            19 => Self::Remove,
            20 => Self::SetMetadata,
            _ => return Err(Error::Invalid)
        })
    }
}
/// Every request is an exact schema header followed by one bounded payload.
/// Integers are little endian. No pointers, Rust layouts or heap ownership cross
/// the module boundary. `token` is supplied by the resident object facility.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Arguments<'a>
{
    pub volume:    u32,
    pub node:      u32,
    pub token:     u32,
    pub parent:    u32,
    pub flags:     u32,
    pub offset:    u64,
    pub length:    u64,
    pub auxiliary: u64,
    pub when:      u64,
    pub payload:   &'a [u8]
}
impl<'a> Arguments<'a>
{
    pub fn encode(&self, output: &mut [u8]) -> Result<usize, Error>
    {
        if self.payload.len() > MAX_TRANSFER
        {
            return Err(Error::Length);
        }
        let count = HEADER_BYTES + self.payload.len();
        let out = output.get_mut(..count).ok_or(Error::Length)?;
        out[..HEADER_BYTES].fill(0);
        put16(out, 0, SCHEMA);
        put16(out, 2, HEADER_BYTES as u16);
        for (at, value) in [(4, self.volume),
                            (8, self.node),
                            (12, self.token),
                            (16, self.parent),
                            (20, self.flags)]
        {
            put32(out, at, value)
        }
        for (at, value) in
            [(24, self.offset), (32, self.length), (40, self.auxiliary), (48, self.when)]
        {
            put64(out, at, value)
        }
        put32(out, 56, self.payload.len() as u32);
        out[HEADER_BYTES..].copy_from_slice(self.payload);
        Ok(count)
    }

    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error>
    {
        if bytes.len() < HEADER_BYTES || bytes.len() > MAX_REQUEST
        {
            return Err(Error::Length);
        }
        if get16(bytes, 0) != SCHEMA
        {
            return Err(Error::Version);
        }
        if get16(bytes, 2) as usize != HEADER_BYTES || get32(bytes, 60) != 0
        {
            return Err(Error::Invalid);
        }
        if get32(bytes, 56) as usize != bytes.len() - HEADER_BYTES
        {
            return Err(Error::Length);
        }
        Ok(Self { volume:    get32(bytes, 4),
                  node:      get32(bytes, 8),
                  token:     get32(bytes, 12),
                  parent:    get32(bytes, 16),
                  flags:     get32(bytes, 20),
                  offset:    get64(bytes, 24),
                  length:    get64(bytes, 32),
                  auxiliary: get64(bytes, 40),
                  when:      get64(bytes, 48),
                  payload:   &bytes[HEADER_BYTES..] })
    }

    pub fn name(&self) -> Result<&'a str, Error>
    {
        name(self.payload)
    }
}
pub(crate) fn name(bytes: &[u8]) -> Result<&str, Error>
{
    let text = core::str::from_utf8(bytes).map_err(|_| Error::Utf8)?;
    if bytes.is_empty()
       || bytes.len() > 255
       || matches!(text, "." | "..")
       || text.contains(['\0', '/', ':'])
    {
        return Err(Error::Invalid);
    }
    Ok(text)
}
pub(crate) fn get16(b: &[u8], at: usize) -> u16
{
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
pub(crate) fn get32(b: &[u8], at: usize) -> u32
{
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
pub(crate) fn get64(b: &[u8], at: usize) -> u64
{
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
pub(crate) fn put16(b: &mut [u8], at: usize, v: u16)
{
    b[at..at + 2].copy_from_slice(&v.to_le_bytes())
}
pub(crate) fn put32(b: &mut [u8], at: usize, v: u32)
{
    b[at..at + 4].copy_from_slice(&v.to_le_bytes())
}
pub(crate) fn put64(b: &mut [u8], at: usize, v: u64)
{
    b[at..at + 8].copy_from_slice(&v.to_le_bytes())
}
