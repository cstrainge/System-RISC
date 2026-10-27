//! SCEX v1: resident Scrap observation and compare/import contract for modules.
//! Labels and raw handles are untrusted inputs; System authenticates the actual
//! declaring module and validates every session and snapshot incarnation.
#![no_std]
#![forbid(unsafe_code)]
use mac_module::ClassId;

pub const CLASS: ClassId = ClassId(*b"SCEX");
pub const VERSION: u32 = 1;
pub const OPEN: u32 = 1;
pub const NOTICE: u32 = 2;
pub const SNAPSHOT: u32 = 3;
pub const READ: u32 = 4;
pub const IMPORT: u32 = 5;
pub const CLOSE_PIN: u32 = 6;
pub const DISCONNECT: u32 = 7;
pub const INFO: u32 = 8;
pub const MAX_REQUEST: usize = 1024 * 1024;
pub const MAX_REPLY: usize = 4096;
pub const MAX_TEXT: usize = MAX_REQUEST - 32;
pub const REPLY_HEADER: usize = 8;
/// Every call provides enough reply space for a useful cause as well as its
/// success result. This is checked before the resident commits any effect.
pub const MIN_REPLY: usize = 512;
pub const METADATA_BYTES: usize = 16;
pub const PIN_BYTES: usize = 8;
pub const READ_BYTES: usize = MAX_REPLY - REPLY_HEADER;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Content
{
    Empty,
    Text,
    Unsupported
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata
{
    pub revision: u64,
    pub content:  Content,
    pub bytes:    u32
}
impl Metadata
{
    pub fn encode(self) -> [u8; METADATA_BYTES]
    {
        let mut out = [0; METADATA_BYTES];
        out[..8].copy_from_slice(&self.revision.to_le_bytes());
        out[8..12].copy_from_slice(&(match self.content
                                    {
                                        Content::Empty => 0u32,
                                        Content::Text => 1,
                                        Content::Unsupported => 2
                                    }).to_le_bytes());
        out[12..].copy_from_slice(&self.bytes.to_le_bytes());
        out
    }

    pub fn decode(input: &[u8]) -> Option<Self>
    {
        if input.len() != METADATA_BYTES
        {
            return None;
        }
        let revision = u64::from_le_bytes(input[..8].try_into().ok()?);
        let content = match u32::from_le_bytes(input[8..12].try_into().ok()?)
        {
            0 => Content::Empty,
            1 => Content::Text,
            2 => Content::Unsupported,
            _ => return None
        };
        let bytes = u32::from_le_bytes(input[12..].try_into().ok()?);
        if revision == 0 || content != Content::Text && bytes != 0
        {
            return None;
        }
        Some(Self { revision,
                    content,
                    bytes })
    }
}
/// A negative status carries a UTF-8 diagnostic from the resident typed cause.
/// Such a result is a terminal reply, not a broker retry ticket. Successful
/// payloads use the operation's fixed layout or a bounded slice of text bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply<'a>
{
    Success(&'a [u8]),
    Failure
    {
        status: i32,
        cause:  &'a str
    }
}
impl<'a> Reply<'a>
{
    pub fn decode(input: &'a [u8]) -> Option<Self>
    {
        let header = input.get(..REPLY_HEADER)?;
        let status = i32::from_le_bytes(header[..4].try_into().ok()?);
        let bytes = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
        let body = input.get(REPLY_HEADER..)?;
        if input.len() > MAX_REPLY || body.len() != bytes || status > 0
        {
            return None;
        }
        if status == 0
        {
            Some(Self::Success(body))
        }
        else
        {
            let cause = core::str::from_utf8(body).ok()?;
            if cause.is_empty()
            {
                return None;
            }
            Some(Self::Failure { status,
                                 cause })
        }
    }
}
