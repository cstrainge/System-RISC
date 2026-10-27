//! Safe SCEX adapter shared by the native broker and host conformance fixture.
//! Every invocation owns fresh authentication and borrows buffers only until
//! return. Diagnostics remain attached to the typed operation's terminal reply.
#![forbid(unsafe_code)]
use core::fmt::{self, Write};

use mac_abi::{BAD_ARGUMENT, NOT_SUPPORTED, Status};
use mac_scrap_extension_interface as wire;

use super::{Manager,
            extension::{self, Authority, Pin, Session}};

struct Diagnostic<'a>
{
    output: &'a mut [u8],
    bytes:  usize
}
impl Write for Diagnostic<'_>
{
    fn write_str(&mut self, value: &str) -> fmt::Result
    {
        let mut bytes = value.len().min(self.output.len() - self.bytes);
        while !value.is_char_boundary(bytes)
        {
            bytes -= 1;
        }
        self.output[self.bytes..self.bytes + bytes].copy_from_slice(&value.as_bytes()[..bytes]);
        self.bytes += bytes;
        if bytes == value.len() { Ok(()) } else { Err(fmt::Error) }
    }
}
enum Fault
{
    Arguments(&'static str),
    Unsupported,
    Service(extension::Error)
}
impl From<extension::Error> for Fault
{
    fn from(cause: extension::Error) -> Self
    {
        Self::Service(cause)
    }
}
impl Fault
{
    fn status(&self) -> Status
    {
        match self
        {
            Self::Arguments(_) => BAD_ARGUMENT,
            Self::Unsupported => NOT_SUPPORTED,
            Self::Service(cause) => cause.status()
        }
    }
}
impl fmt::Display for Fault
{
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Arguments(detail) =>
            {
                write!(output,
                       "SCEX request refused: {detail}; rebuild the extension against the current \
                        Scrap service contract")
            }
            Self::Unsupported =>
            {
                output.write_str("SCEX operation is unsupported; rebuild the extension against \
                                  the current System Scrap contract")
            }
            Self::Service(cause) => cause.fmt(output)
        }
    }
}
fn word(input: &[u8]) -> Result<u64, Fault>
{
    Ok(u64::from_le_bytes(input.try_into().map_err(|_| {
                                               Fault::Arguments("a handle or offset must contain \
                                                                 exactly eight bytes")
                                           })?))
}
fn pair(input: &[u8]) -> Result<(Session, Pin), Fault>
{
    if input.len() != 16
    {
        return Err(Fault::Arguments("session and snapshot fields must contain exactly sixteen \
                                     bytes"));
    }
    Ok((Session::from_raw(word(&input[..8])?), Pin::from_raw(word(&input[8..])?)))
}
fn metadata(info: extension::Read) -> wire::Metadata
{
    wire::Metadata { revision: info.revision.get(),
                     content:  match info.content
                     {
                         extension::Content::Empty => wire::Content::Empty,
                         extension::Content::Text => wire::Content::Text,
                         extension::Content::Unsupported => wire::Content::Unsupported
                     },
                     bytes:    info.total as u32 }
}
fn operation(manager: &mut Manager, authority: &Authority, operation: u32, input: &[u8],
             output: &mut [u8])
             -> Result<usize, Fault>
{
    match operation
    {
        wire::OPEN =>
        {
            let writable = match input
            {
                [0, 0, 0, 0] => false,
                [1, 0, 0, 0] => true,
                _ => return Err(Fault::Arguments("OPEN requires one checked writable flag"))
            };
            let session = manager.extension_open(authority, writable)?;
            output[..8].copy_from_slice(&session.raw().to_le_bytes());
            Ok(8)
        }
        wire::NOTICE =>
        {
            let session = Session::from_raw(word(input)?);
            let revision =
                manager.extension_notice(authority, session)?.map_or(0, |revision| revision.get());
            output[..8].copy_from_slice(&revision.to_le_bytes());
            Ok(8)
        }
        wire::SNAPSHOT =>
        {
            let session = Session::from_raw(word(input)?);
            let pin = manager.extension_snapshot(authority, session)?;
            output[..8].copy_from_slice(&pin.raw().to_le_bytes());
            Ok(8)
        }
        wire::INFO =>
        {
            let (session, pin) = pair(input)?;
            let info = manager.extension_info(authority, session, pin)?;
            output[..wire::METADATA_BYTES].copy_from_slice(&metadata(info).encode());
            Ok(wire::METADATA_BYTES)
        }
        wire::READ =>
        {
            if input.len() != 24
            {
                return Err(Fault::Arguments("READ requires session, snapshot and byte offset"));
            }
            let (session, pin) = pair(&input[..16])?;
            let offset = usize::try_from(word(&input[16..])?).map_err(|_| {
                                                                 Fault::Arguments("READ offset \
                                                                                   exceeds native \
                                                                                   address width")
                                                             })?;
            let info = manager.extension_read(authority, session, pin, offset, output)?;
            Ok(info.copied)
        }
        wire::IMPORT =>
        {
            let prefix =
                input.get(..16).ok_or(Fault::Arguments("IMPORT requires the original session \
                                                         and snapshot pin"))?;
            let (session, pin) = pair(prefix)?;
            let text = core::str::from_utf8(&input[16..]).map_err(|_| {
                                                             Fault::Arguments("IMPORT text must \
                                                                               be valid UTF-8; \
                                                                               current Scrap was \
                                                                               retained")
                                                         })?;
            let revision = manager.extension_import(authority, session, pin, text)?;
            output[..8].copy_from_slice(&revision.get().to_le_bytes());
            Ok(8)
        }
        wire::CLOSE_PIN =>
        {
            let (session, pin) = pair(input)?;
            manager.extension_close_pin(authority, session, pin)?;
            Ok(0)
        }
        wire::DISCONNECT =>
        {
            let session = Session::from_raw(word(input)?);
            manager.extension_disconnect(authority, session)?;
            Ok(0)
        }
        _ => Err(Fault::Unsupported)
    }
}

/// Encode an admission/access failure without consuming a session or pin.
pub(crate) fn refuse(status: Status, cause: &impl fmt::Display, output: &mut [u8])
                     -> Result<usize, Status>
{
    if !(wire::MIN_REPLY..=wire::MAX_REPLY).contains(&output.len()) || status >= 0
    {
        return Err(BAD_ARGUMENT);
    }
    let mut diagnostic = Diagnostic { output: &mut output[wire::REPLY_HEADER..],
                                      bytes:  0 };
    let _ = write!(diagnostic, "{cause}");
    let bytes = diagnostic.bytes;
    output[..4].copy_from_slice(&status.to_le_bytes());
    output[4..wire::REPLY_HEADER].copy_from_slice(&(bytes as u32).to_le_bytes());
    Ok(wire::REPLY_HEADER + bytes)
}

pub(crate) fn dispatch(manager: &mut Manager, authority: &Authority, op: u32, input: &[u8],
                       output: &mut [u8])
                       -> Result<usize, Status>
{
    if input.len() > wire::MAX_REQUEST
       || !(wire::MIN_REPLY..=wire::MAX_REPLY).contains(&output.len())
    {
        return Err(BAD_ARGUMENT);
    }
    let result = operation(manager, authority, op, input, &mut output[wire::REPLY_HEADER..]);
    let (status, bytes) = match result
    {
        Ok(bytes) => (0, bytes),
        Err(cause) =>
        {
            let mut diagnostic = Diagnostic { output: &mut output[wire::REPLY_HEADER..],
                                              bytes:  0 };
            let _ = write!(diagnostic, "{cause}");
            (cause.status(), diagnostic.bytes)
        }
    };
    output[..4].copy_from_slice(&status.to_le_bytes());
    output[4..wire::REPLY_HEADER].copy_from_slice(&(bytes as u32).to_le_bytes());
    Ok(wire::REPLY_HEADER + bytes)
}
