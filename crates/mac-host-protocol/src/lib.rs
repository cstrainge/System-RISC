//! Bounded byte protocol for the VM's explicitly exported host capabilities.
//!
//! This layer transports directory operations and file bytes, not Macintosh
//! catalog semantics. Fork containers belong to the filesystem provider. A
//! packet's correlation ID is the guest broker's live request ticket; it never
//! authorizes access. Session/object capabilities are validated by the server.
#![no_std]
#![forbid(unsafe_code)]

pub const VERSION: u16 = 1;
pub const HEADER_BYTES: usize = 48;
pub const MAX_PAYLOAD: usize = 64 * 1024;
pub const MAGIC: [u8; 8] = *b"RSHOST01";
pub const LOCK_SHARED: u32 = 0;
pub const LOCK_EXCLUSIVE: u32 = 1;
pub const READ: u32 = 1;
pub const WRITE: u32 = 2;
pub const DURABLE: u32 = 4;
pub const ATOMIC_PUBLISH: u32 = 8;
pub const ALL_CAPABILITIES: u32 = READ | WRITE | DURABLE | ATOMIC_PUBLISH;

/// Wire status values, independent of CPU ABI and native Toolbox error numbers.
pub mod status
{
    pub const OK: i32 = 0;
    pub const INVALID: i32 = -1;
    pub const STALE: i32 = -2;
    pub const UNAVAILABLE: i32 = -3;
    pub const READ_ONLY: i32 = -4;
    pub const CONFLICT: i32 = -5;
    pub const LIMIT: i32 = -6;
    pub const IO: i32 = -7;
    pub const NOT_FOUND: i32 = -8;
    pub const BUSY: i32 = -9;
    pub const UNSUPPORTED: i32 = -10;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum Operation
{
    Hello         = 1,
    Export        = 2,
    OpenDirectory = 3,
    ListBegin     = 4,
    ListNext      = 5,
    OpenRead      = 6,
    Read          = 7,
    Close         = 8,
    BeginWrite    = 9,
    Write         = 10,
    SetLength     = 11,
    Flush         = 12,
    Commit        = 13,
    MakeDirectory = 14,
    Rename        = 15,
    Remove        = 16,
    SyncDirectory = 17,
    OpenLock      = 18,
    Lock          = 19,
    Unlock        = 20,
    Stat          = 21
}
impl TryFrom<u16> for Operation
{
    type Error = Error;

    fn try_from(value: u16) -> Result<Self, Error>
    {
        Ok(match value
        {
            1 => Self::Hello,
            2 => Self::Export,
            3 => Self::OpenDirectory,
            4 => Self::ListBegin,
            5 => Self::ListNext,
            6 => Self::OpenRead,
            7 => Self::Read,
            8 => Self::Close,
            9 => Self::BeginWrite,
            10 => Self::Write,
            11 => Self::SetLength,
            12 => Self::Flush,
            13 => Self::Commit,
            14 => Self::MakeDirectory,
            15 => Self::Rename,
            16 => Self::Remove,
            17 => Self::SyncDirectory,
            18 => Self::OpenLock,
            19 => Self::Lock,
            20 => Self::Unlock,
            21 => Self::Stat,
            _ => return Err(Error::Operation)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Header,
    Version,
    Operation,
    Length,
    Component
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header
{
    pub operation: Operation,
    pub reply:     bool,
    pub payload:   u32,
    pub request:   u32,
    pub session:   u32,
    pub object:    u32,
    pub offset:    u64,
    pub value:     u32,
    pub status:    i32
}
impl Header
{
    pub fn request(operation: Operation, request: u32, session: u32, object: u32) -> Self
    {
        Self { operation,
               reply: false,
               payload: 0,
               request,
               session,
               object,
               offset: 0,
               value: 0,
               status: status::OK }
    }

    pub fn response(self) -> Self
    {
        Self { reply: true,
               payload: 0,
               object: 0,
               offset: 0,
               value: 0,
               status: status::OK,
               ..self }
    }

    fn validate(self) -> Result<(), Error>
    {
        if self.payload as usize > MAX_PAYLOAD
        {
            return Err(Error::Length);
        }
        if self.request == 0
           || !self.reply && self.status != status::OK
           || self.operation != Operation::Hello && self.session == 0
        {
            return Err(Error::Header);
        }
        Ok(())
    }

    pub fn encode(self) -> Result<[u8; HEADER_BYTES], Error>
    {
        self.validate()?;
        let mut bytes = [0; HEADER_BYTES];
        bytes[..8].copy_from_slice(&MAGIC);
        bytes[8..10].copy_from_slice(&VERSION.to_le_bytes());
        bytes[10..12].copy_from_slice(&(HEADER_BYTES as u16).to_le_bytes());
        bytes[12..16].copy_from_slice(&self.payload.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.request.to_le_bytes());
        bytes[20..22].copy_from_slice(&(self.operation as u16).to_le_bytes());
        bytes[22..24].copy_from_slice(&u16::from(self.reply).to_le_bytes());
        bytes[24..28].copy_from_slice(&self.session.to_le_bytes());
        bytes[28..32].copy_from_slice(&self.object.to_le_bytes());
        bytes[32..40].copy_from_slice(&self.offset.to_le_bytes());
        bytes[40..44].copy_from_slice(&self.value.to_le_bytes());
        bytes[44..48].copy_from_slice(&self.status.to_le_bytes());
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, Error>
    {
        if bytes.len() != HEADER_BYTES
           || bytes[..8] != MAGIC
           || bytes[10..12] != (HEADER_BYTES as u16).to_le_bytes()
           || u16::from_le_bytes(bytes[22..24].try_into().unwrap()) > 1
        {
            return Err(Error::Header);
        }
        if bytes[8..10] != VERSION.to_le_bytes()
        {
            return Err(Error::Version);
        }
        let header = Self { payload:   u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
                            request:   u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
                            operation:
                                Operation::try_from(u16::from_le_bytes(bytes[20..22].try_into()
                                                                                    .unwrap()))?,
                            reply:     bytes[22] != 0,
                            session:   u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
                            object:    u32::from_le_bytes(bytes[28..32].try_into().unwrap()),
                            offset:    u64::from_le_bytes(bytes[32..40].try_into().unwrap()),
                            value:     u32::from_le_bytes(bytes[40..44].try_into().unwrap()),
                            status:    i32::from_le_bytes(bytes[44..48].try_into().unwrap()) };
        header.validate()?;
        Ok(header)
    }
}

/// Paths are directory capabilities plus one portable host component. There is
/// no ambient/absolute path form and no dot traversal in the wire protocol.
pub fn component(bytes: &[u8]) -> Result<&str, Error>
{
    let name = core::str::from_utf8(bytes).map_err(|_| Error::Component)?;
    if bytes.is_empty()
       || bytes.len() > 255
       || matches!(name, "." | "..")
       || name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
    {
        return Err(Error::Component);
    }
    Ok(name)
}

/// Rename carries two components separated by a little-endian u16 first length.
pub fn rename_names(bytes: &[u8]) -> Result<(&str, &str), Error>
{
    let size = bytes.get(..2).ok_or(Error::Length)?;
    let size = u16::from_le_bytes(size.try_into().unwrap()) as usize;
    let first = bytes.get(2..2 + size).ok_or(Error::Length)?;
    Ok((component(first)?, component(&bytes[2 + size..])?))
}

/// A single in-flight reply, assembled without allocation. Storage remains owned
/// by the transport; no caller buffer is retained between polling callbacks.
/// A malformed reply poisons this receiver until a new connection/request is
/// explicitly established. Extra bytes are left to the transport to reject.
pub struct Receiver
{
    header:      [u8; HEADER_BYTES],
    header_used: usize,
    body_used:   usize,
    parsed:      Option<Header>,
    request:     u32,
    operation:   Operation,
    failed:      bool
}
impl Receiver
{
    pub const fn new(request: u32, operation: Operation) -> Self
    {
        Self { header: [0; HEADER_BYTES],
               header_used: 0,
               body_used: 0,
               parsed: None,
               request,
               operation,
               failed: false }
    }

    pub fn completed(&self) -> Option<Header>
    {
        if self.failed
        {
            return None;
        }
        self.parsed.filter(|h| self.body_used == h.payload as usize)
    }

    pub fn feed(&mut self, bytes: &[u8], body: &mut [u8]) -> Result<usize, Error>
    {
        if self.failed
        {
            return Err(Error::Header);
        }
        let result = self.feed_inner(bytes, body);
        if result.is_err()
        {
            self.failed = true;
        }
        result
    }

    fn feed_inner(&mut self, bytes: &[u8], body: &mut [u8]) -> Result<usize, Error>
    {
        if self.completed().is_some()
        {
            return Ok(0);
        }
        let header_count = (HEADER_BYTES - self.header_used).min(bytes.len());
        if header_count != 0
        {
            self.header[self.header_used..self.header_used+header_count].copy_from_slice(&bytes[..header_count]);
            self.header_used += header_count;
        }
        if self.header_used != HEADER_BYTES
        {
            return Ok(header_count);
        }
        let header = match self.parsed
        {
            Some(header) => header,
            None =>
            {
                let header = Header::decode(&self.header)?;
                if !header.reply
                   || header.request != self.request
                   || header.operation != self.operation
                {
                    return Err(Error::Header);
                }
                if header.payload as usize > body.len()
                {
                    return Err(Error::Length);
                }
                self.parsed = Some(header);
                header
            }
        };
        let count = (header.payload as usize - self.body_used).min(bytes.len() - header_count);
        let end = self.body_used.checked_add(count).ok_or(Error::Length)?;
        let target = body.get_mut(self.body_used..end).ok_or(Error::Length)?;
        target.copy_from_slice(&bytes[header_count..header_count + count]);
        self.body_used = end;
        Ok(header_count + count)
    }
}

/// Kernel-module directory class. The broker supplies operation and correlation
/// ticket; class input contains only session/object arguments and payload. The
/// transport creates the actual wire header after it receives its live ticket.
pub const DIRECTORY_CLASS: [u8; 4] = *b"HDIR";
pub const DIRECTORY_VERSION: u32 = 1;
pub const ARGUMENT_BYTES: usize = 24;
pub struct Arguments<'a>
{
    pub session: u32,
    pub object:  u32,
    pub offset:  u64,
    pub value:   u32,
    pub payload: &'a [u8]
}
impl<'a> Arguments<'a>
{
    pub fn encode(&self, output: &mut [u8]) -> Result<usize, Error>
    {
        if self.payload.len() > MAX_PAYLOAD
        {
            return Err(Error::Length);
        }
        let length = ARGUMENT_BYTES + self.payload.len();
        let output = output.get_mut(..length).ok_or(Error::Length)?;
        output[..4].copy_from_slice(&self.session.to_le_bytes());
        output[4..8].copy_from_slice(&self.object.to_le_bytes());
        output[8..16].copy_from_slice(&self.offset.to_le_bytes());
        output[16..20].copy_from_slice(&self.value.to_le_bytes());
        output[20..24].copy_from_slice(&(self.payload.len() as u32).to_le_bytes());
        output[24..].copy_from_slice(self.payload);
        Ok(length)
    }

    pub fn decode(bytes: &'a [u8]) -> Result<Self, Error>
    {
        if bytes.len() < ARGUMENT_BYTES || bytes.len() > ARGUMENT_BYTES + MAX_PAYLOAD
        {
            return Err(Error::Length);
        }
        if u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as usize
           != bytes.len() - ARGUMENT_BYTES
        {
            return Err(Error::Length);
        }
        Ok(Self { session: u32::from_le_bytes(bytes[..4].try_into().unwrap()),
                  object:  u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
                  offset:  u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
                  value:   u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
                  payload: &bytes[24..] })
    }

    pub fn header(&self, operation: Operation, ticket: u32) -> Result<Header, Error>
    {
        let header = Header { offset: self.offset,
                              value: self.value,
                              payload: self.payload.len() as u32,
                              ..Header::request(operation, ticket, self.session, self.object) };
        header.validate()?;
        Ok(header)
    }
}
