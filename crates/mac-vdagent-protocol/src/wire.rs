use core::fmt;

pub const PROTOCOL: u32 = 1;
pub const PORT_NAME: &str = "com.redhat.spice.0";
pub const CHUNK_HEADER_BYTES: usize = 8;
pub const MESSAGE_HEADER_BYTES: usize = 20;
/// External SPICE VDI chunk payload bound; QEMU currently emits 1024-byte chunks.
pub const MAX_CHUNK_BYTES: usize = 2048;
/// QEMU's vdagent output queue admits a complete message against a 1 MiB bound.
/// This includes the message header and clipboard prefixes, not just text.
pub const QEMU_MAX_MESSAGE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error
{
    Length,
    Protocol
    {
        observed: u32
    },
    ChunkPort
    {
        observed: u32
    },
    ChunkSize
    {
        observed: u32
    },
    MessageSize
    {
        bytes:   u64,
        maximum: usize
    },
    Limits,
    Layout
    {
        message: MessageType,
        reason:  &'static str
    },
    CapabilityRequired(Capability),
    Selection
    {
        observed: u8
    },
    UnexpectedSerial,
    MissingSerial,
    Utf8,
    UnsupportedFormat
    {
        observed: u32
    },
    Maximum
    {
        observed: i64
    },
    NoMemory
    {
        bytes: usize
    },
    Poisoned
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        f.write_str("Host Clipboard vdagent ")?;
        match self
        {
            Self::NoMemory { bytes } =>
            {
                return write!(f,
                              "cannot reserve {bytes} receive bytes; retain the current Scrap \
                               and retry when memory is available");
            }
            Self::Length => f.write_str("received truncated or surplus wire fields")?,
            Self::Protocol { observed } => write!(f, "received unsupported protocol {observed}")?,
            Self::ChunkPort { observed } =>
            {
                write!(f, "received unsupported channel port {observed}")?
            }
            Self::ChunkSize { observed } =>
            {
                write!(f, "received an invalid chunk length {observed}")?
            }
            Self::MessageSize { bytes,
                                maximum } =>
            {
                write!(f,
                       "refused a {bytes}-byte message exceeding its {maximum}-byte admission \
                        bound")?
            }
            Self::Limits => f.write_str("received invalid local message limits")?,
            Self::Layout { message,
                           reason } =>
            {
                write!(f, "refused message {} because {reason}", message.raw())?
            }
            Self::CapabilityRequired(capability) =>
            {
                write!(f, "received a message requiring unnegotiated {capability:?}")?
            }
            Self::Selection { observed } =>
            {
                write!(f, "received an unsupported selection {observed}")?
            }
            Self::UnexpectedSerial =>
            {
                f.write_str("received a grab serial without negotiated serial support")?
            }
            Self::MissingSerial => f.write_str("requires the negotiated grab serial")?,
            Self::Utf8 => f.write_str("received invalid UTF-8 text")?,
            Self::UnsupportedFormat { observed } =>
            {
                write!(f, "does not implement clipboard format {observed}")?
            }
            Self::Maximum { observed } =>
            {
                write!(f, "received an invalid clipboard maximum {observed}")?
            }
            Self::Poisoned => f.write_str("cannot continue after malformed framing")?
        }
        f.write_str("; retain the current Scrap, reset the channel and negotiate a compatible \
                     endpoint")
    }
}
impl core::error::Error for Error
{
}
pub type Result<T> = core::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits
{
    maximum: usize
}
impl Limits
{
    pub fn new(maximum_message_bytes: usize) -> Result<Self>
    {
        if !(MESSAGE_HEADER_BYTES..=QEMU_MAX_MESSAGE_BYTES).contains(&maximum_message_bytes)
        {
            return Err(Error::Limits);
        }
        Ok(Self { maximum: maximum_message_bytes })
    }

    pub const fn qemu() -> Self
    {
        Self { maximum: QEMU_MAX_MESSAGE_BYTES }
    }

    pub fn maximum_message_bytes(self) -> usize
    {
        self.maximum
    }

    pub fn maximum_text_bytes(self, capabilities: Capabilities) -> usize
    {
        self.maximum.saturating_sub(MESSAGE_HEADER_BYTES
                                    + 4
                                    + if capabilities.contains(Capability::ClipboardSelection)
                                    {
                                        4
                                    }
                                    else
                                    {
                                        0
                                    })
    }

    pub(crate) fn admit(self, payload: usize) -> Result<()>
    {
        let total = (payload as u64).checked_add(MESSAGE_HEADER_BYTES as u64).unwrap_or(u64::MAX);
        if total > self.maximum as u64
        {
            return Err(Error::MessageSize { bytes:   total,
                                            maximum: self.maximum });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Port
{
    Client = 1,
    Server = 2
}
impl TryFrom<u32> for Port
{
    type Error = Error;

    fn try_from(observed: u32) -> Result<Self>
    {
        match observed
        {
            1 => Ok(Self::Client),
            2 => Ok(Self::Server),
            _ => Err(Error::ChunkPort { observed })
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkHeader
{
    port: Port,
    size: u32
}
impl ChunkHeader
{
    pub fn new(port: Port, size: usize) -> Result<Self>
    {
        if size > MAX_CHUNK_BYTES
        {
            return Err(Error::ChunkSize { observed: u32::try_from(size).unwrap_or(u32::MAX) });
        }
        Ok(Self { port,
                  size: size as u32 })
    }

    pub fn decode(bytes: &[u8]) -> Result<Self>
    {
        if bytes.len() != CHUNK_HEADER_BYTES
        {
            return Err(Error::Length);
        }
        Self::new(Port::try_from(le32(bytes, 0))?, le32(bytes, 4) as usize)
    }

    pub fn encode(self) -> [u8; CHUNK_HEADER_BYTES]
    {
        let mut bytes = [0; CHUNK_HEADER_BYTES];
        bytes[..4].copy_from_slice(&(self.port as u32).to_le_bytes());
        bytes[4..].copy_from_slice(&self.size.to_le_bytes());
        bytes
    }

    pub fn port(self) -> Port
    {
        self.port
    }

    pub fn size(self) -> usize
    {
        self.size as usize
    }
}

/// A positive, admitted encoder chunk policy. Shorter fragments preserve the
/// same ChunkHeader rule and use its sole wire encoder.
pub(crate) struct ChunkPolicy
{
    maximum: ChunkHeader
}
impl ChunkPolicy
{
    pub(crate) fn new(maximum: usize) -> Result<Self>
    {
        let maximum = ChunkHeader::new(Port::Client, maximum)?;
        if maximum.size() == 0
        {
            return Err(Error::ChunkSize { observed: 0 });
        }
        Ok(Self { maximum })
    }

    pub(crate) fn maximum(&self) -> usize
    {
        self.maximum.size()
    }

    pub(crate) fn fragment(&self, remaining: usize) -> Option<ChunkHeader>
    {
        let size = self.maximum.size().min(remaining);
        if size == 0
        {
            None
        }
        else
        {
            Some(ChunkHeader { port: Port::Client,
                               size: size as u32 })
        }
    }
}

/// External message kind, never an object ID or an invocation capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageType(u32);
impl MessageType
{
    pub const ANNOUNCE_CAPABILITIES: Self = Self(6);
    pub const CLIENT_DISCONNECTED: Self = Self(13);
    pub const CLIPBOARD: Self = Self(4);
    pub const CLIPBOARD_GRAB: Self = Self(7);
    pub const CLIPBOARD_RELEASE: Self = Self(9);
    pub const CLIPBOARD_REQUEST: Self = Self(8);
    pub const MAX_CLIPBOARD: Self = Self(14);

    pub const fn from_raw(value: u32) -> Self
    {
        Self(value)
    }

    pub fn raw(self) -> u32
    {
        self.0
    }

    pub(crate) fn retained(self) -> bool
    {
        matches!(self,
                 Self::CLIPBOARD
                 | Self::ANNOUNCE_CAPABILITIES
                 | Self::CLIPBOARD_GRAB
                 | Self::CLIPBOARD_REQUEST
                 | Self::CLIPBOARD_RELEASE
                 | Self::CLIENT_DISCONNECTED
                 | Self::MAX_CLIPBOARD)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header
{
    kind:   MessageType,
    opaque: u64,
    size:   u32
}
impl Header
{
    pub fn new(kind: MessageType, payload_bytes: usize, limits: Limits) -> Result<Self>
    {
        limits.admit(payload_bytes)?;
        Ok(Self { kind,
                  opaque: 0,
                  size: payload_bytes as u32 })
    }

    pub fn decode(bytes: &[u8], limits: Limits) -> Result<Self>
    {
        if bytes.len() != MESSAGE_HEADER_BYTES
        {
            return Err(Error::Length);
        }
        let protocol = le32(bytes, 0);
        if protocol != PROTOCOL
        {
            return Err(Error::Protocol { observed: protocol });
        }
        let size = le32(bytes, 16);
        limits.admit(size as usize)?;
        let mut opaque = [0; 8];
        opaque.copy_from_slice(&bytes[8..16]);
        Ok(Self { kind: MessageType(le32(bytes, 4)),
                  opaque: u64::from_le_bytes(opaque),
                  size })
    }

    pub fn encode(self) -> [u8; MESSAGE_HEADER_BYTES]
    {
        let mut bytes = [0; MESSAGE_HEADER_BYTES];
        bytes[..4].copy_from_slice(&PROTOCOL.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.kind.0.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.opaque.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.size.to_le_bytes());
        bytes
    }

    pub fn kind(self) -> MessageType
    {
        self.kind
    }

    pub fn payload_bytes(self) -> usize
    {
        self.size as usize
    }

    /// Standard opaque delivery field. Clipboard messages provide no useful
    /// response correlation token here; the bridge must not mint authority from it.
    pub fn opaque(self) -> u64
    {
        self.opaque
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Capability
{
    Clipboard                  = 3,
    ClipboardByDemand          = 5,
    ClipboardSelection         = 6,
    GuestLineEndLf             = 8,
    GuestLineEndCrLf           = 9,
    MaxClipboard               = 10,
    ClipboardNoReleaseOnRegrab = 16,
    ClipboardGrabSerial        = 17
}
const KNOWN_CAPABILITIES: u32 =
    (1 << 3) | (1 << 5) | (1 << 6) | (1 << 8) | (1 << 9) | (1 << 10) | (1 << 16) | (1 << 17);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities(u32);
impl Capabilities
{
    pub const fn empty() -> Self
    {
        Self(0)
    }

    pub const fn with(self, capability: Capability) -> Self
    {
        Self(self.0 | (1u32 << (capability as u8)))
    }

    pub fn contains(self, capability: Capability) -> bool
    {
        self.0 & (1u32 << (capability as u8)) != 0
    }

    pub fn bits(self) -> u32
    {
        self.0
    }

    pub fn from_known_bits(bits: u32) -> Result<Self>
    {
        if bits & !KNOWN_CAPABILITIES != 0
        {
            return Err(layout(MessageType::ANNOUNCE_CAPABILITIES,
                              "local capabilities contain unknown bits"));
        }
        Ok(Self(bits))
    }
}
#[derive(Clone, Copy, Debug)]
pub struct PeerCapabilities<'a>
{
    request: bool,
    words:   &'a [u8]
}
impl PeerCapabilities<'_>
{
    fn first_word(self) -> u32
    {
        if self.words.len() < 4 { 0 } else { le32(self.words, 0) }
    }

    pub fn requests_reply(self) -> bool
    {
        self.request
    }

    pub fn supports(self, capability: Capability) -> bool
    {
        self.first_word() & (1u32 << (capability as u8)) != 0
    }

    pub fn negotiate(self, local: Capabilities) -> Capabilities
    {
        Capabilities(local.0 & self.first_word())
    }

    pub fn unknown_bits_present(self) -> bool
    {
        self.first_word() & !KNOWN_CAPABILITIES != 0
        || self.words.get(4..).is_some_and(|words| words.iter().any(|byte| *byte != 0))
    }

    pub fn word_count(self) -> usize
    {
        self.words.len() / 4
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Selection
{
    Clipboard = 0,
    Primary   = 1,
    Secondary = 2
}
impl TryFrom<u8> for Selection
{
    type Error = Error;

    fn try_from(observed: u8) -> Result<Self>
    {
        match observed
        {
            0 => Ok(Self::Clipboard),
            1 => Ok(Self::Primary),
            2 => Ok(Self::Secondary),
            _ => Err(Error::Selection { observed })
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format(u32);
impl Format
{
    pub const NONE: Self = Self(0);
    pub const UTF8_TEXT: Self = Self(1);

    pub const fn from_raw(raw: u32) -> Self
    {
        Self(raw)
    }

    pub fn raw(self) -> u32
    {
        self.0
    }

    pub fn supported(self) -> bool
    {
        matches!(self, Self::NONE | Self::UTF8_TEXT)
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Formats<'a>
{
    bytes: &'a [u8]
}
impl<'a> Formats<'a>
{
    pub fn iter(self) -> impl ExactSizeIterator<Item = Format> + 'a
    {
        self.bytes.chunks_exact(4).map(|part| Format(le32(part, 0)))
    }

    pub fn contains(self, format: Format) -> bool
    {
        self.iter().any(|item| item == format)
    }

    pub fn len(self) -> usize
    {
        self.bytes.len() / 4
    }

    pub fn is_empty(self) -> bool
    {
        self.bytes.is_empty()
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Grab<'a>
{
    selection: Selection,
    serial:    Option<u32>,
    formats:   Formats<'a>
}
impl<'a> Grab<'a>
{
    pub fn selection(self) -> Selection
    {
        self.selection
    }

    pub fn serial(self) -> Option<u32>
    {
        self.serial
    }

    pub fn formats(self) -> Formats<'a>
    {
        self.formats
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaximumClipboard
{
    Unlimited,
    Bytes(u32)
}
impl MaximumClipboard
{
    pub fn from_raw(value: i32) -> Result<Self>
    {
        match value
        {
            -1 => Ok(Self::Unlimited),
            0..=i32::MAX => Ok(Self::Bytes(value as u32)),
            _ => Err(Error::Maximum { observed: i64::from(value) })
        }
    }

    pub fn raw(self) -> Result<i32>
    {
        match self
        {
            Self::Unlimited => Ok(-1),
            Self::Bytes(bytes) =>
            {
                i32::try_from(bytes).map_err(|_| Error::Maximum { observed: i64::from(bytes) })
            }
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub enum ClipboardData<'a>
{
    Text
    {
        selection: Selection, text: &'a str
    },
    Unavailable
    {
        selection: Selection
    },
    Unsupported
    {
        selection: Selection, format: Format, bytes: &'a [u8]
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Message<'a>
{
    Capabilities(PeerCapabilities<'a>),
    Grab(Grab<'a>),
    Request
    {
        selection: Selection,
        format:    Format
    },
    Data(ClipboardData<'a>),
    Release
    {
        selection: Selection
    },
    ClientDisconnected,
    MaximumClipboard(MaximumClipboard),
    Ignored
    {
        kind:  MessageType,
        bytes: usize
    }
}

/// Known payload borrows are tied to the decoder, not the caller's input buffer.
/// Nonclipboard message bodies are intentionally omitted without allocation.
#[derive(Debug)]
pub struct Frame<'a>
{
    pub(crate) header:  Header,
    pub(crate) payload: Option<&'a [u8]>
}
impl<'a> Frame<'a>
{
    pub fn header(&self) -> Header
    {
        self.header
    }

    pub fn message(&self, capabilities: Capabilities) -> Result<Message<'a>>
    {
        let kind = self.header.kind;
        if !kind.retained()
        {
            return Ok(Message::Ignored { kind,
                                         bytes: self.header.payload_bytes() });
        }
        let data = self.payload.ok_or_else(|| layout(kind, "the admitted body is absent"))?;
        if data.len() != self.header.payload_bytes()
        {
            return Err(Error::Length);
        }
        if kind == MessageType::ANNOUNCE_CAPABILITIES
        {
            if data.len() < 4 || (data.len() - 4) % 4 != 0 || le32(data, 0) > 1
            {
                return Err(layout(kind, "capability words or request flag are malformed"));
            }
            return Ok(Message::Capabilities(PeerCapabilities { request: le32(data, 0) != 0,
                                                               words:   &data[4..] }));
        }
        if kind == MessageType::CLIENT_DISCONNECTED
        {
            if !data.is_empty()
            {
                return Err(layout(kind, "disconnect must have an empty body"));
            }
            return Ok(Message::ClientDisconnected);
        }
        if kind == MessageType::MAX_CLIPBOARD
        {
            if !capabilities.contains(Capability::MaxClipboard)
            {
                return Err(Error::CapabilityRequired(Capability::MaxClipboard));
            }
            if data.len() != 4
            {
                return Err(layout(kind, "maximum must contain exactly one signed word"));
            }
            return Ok(Message::MaximumClipboard(MaximumClipboard::from_raw(le32(data, 0)
                                                                           as i32)?));
        }
        if !capabilities.contains(Capability::ClipboardByDemand)
        {
            return Err(Error::CapabilityRequired(Capability::ClipboardByDemand));
        }
        let (selection, data) = selection_prefix(kind, data, capabilities)?;
        if kind == MessageType::CLIPBOARD_RELEASE
        {
            if !data.is_empty()
            {
                return Err(layout(kind, "release has surplus fields"));
            }
            return Ok(Message::Release { selection });
        }
        if kind == MessageType::CLIPBOARD_GRAB
        {
            let (serial, types) = if capabilities.contains(Capability::ClipboardGrabSerial)
            {
                if data.len() < 4
                {
                    return Err(Error::MissingSerial);
                }
                (Some(le32(data, 0)), &data[4..])
            }
            else
            {
                (None, data)
            };
            if types.len() % 4 != 0
            {
                return Err(layout(kind, "grab types do not contain complete words"));
            }
            return Ok(Message::Grab(Grab { selection,
                                           serial,
                                           formats: Formats { bytes: types } }));
        }
        if data.len() < 4
        {
            return Err(layout(kind, "clipboard format word is absent"));
        }
        let format = Format(le32(data, 0));
        let bytes = &data[4..];
        if kind == MessageType::CLIPBOARD_REQUEST
        {
            if !bytes.is_empty()
            {
                return Err(layout(kind, "request has surplus fields"));
            }
            return Ok(Message::Request { selection,
                                         format });
        }
        if kind == MessageType::CLIPBOARD
        {
            let data = match format
            {
                Format::UTF8_TEXT =>
                {
                    ClipboardData::Text { selection,
                                          text: core::str::from_utf8(bytes).map_err(|_| {
                                                                               Error::Utf8
                                                                           })? }
                }
                Format::NONE =>
                {
                    if !bytes.is_empty()
                    {
                        return Err(layout(kind, "unavailable data has surplus bytes"));
                    }
                    ClipboardData::Unavailable { selection }
                }
                _ =>
                {
                    ClipboardData::Unsupported { selection,
                                                 format,
                                                 bytes }
                }
            };
            return Ok(Message::Data(data));
        }
        Ok(Message::Ignored { kind,
                              bytes: data.len() })
    }
}
pub(crate) fn selection_prefix(kind: MessageType, data: &[u8], capabilities: Capabilities)
                               -> Result<(Selection, &[u8])>
{
    if !capabilities.contains(Capability::ClipboardSelection)
    {
        return Ok((Selection::Clipboard, data));
    }
    if data.len() < 4
    {
        return Err(layout(kind, "selection prefix is absent"));
    }
    if data[1..4] != [0; 3]
    {
        return Err(layout(kind, "selection reserved bytes are nonzero"));
    }
    Ok((Selection::try_from(data[0])?, &data[4..]))
}
pub(crate) fn layout(message: MessageType, reason: &'static str) -> Error
{
    Error::Layout { message,
                    reason }
}
pub(crate) fn le32(bytes: &[u8], offset: usize) -> u32
{
    u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
}
