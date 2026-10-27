use alloc::vec::Vec;

use crate::{Capabilities, Capability, Error, Format, Frame, Header, Limits, MESSAGE_HEADER_BYTES,
            MaximumClipboard, MessageType, Result, Selection, wire::ChunkPolicy};

/// Queued work owns its bytes. Iterators/views borrow that owner, so a transport
/// cannot retain a slice after the corresponding queued message is destroyed.
///
/// ```compile_fail
/// use mac_vdagent_protocol::{OwnedMessage,Capabilities,Capability,Limits,Selection};
/// let caps=Capabilities::empty().with(Capability::ClipboardByDemand);
/// let message=OwnedMessage::text(Selection::Clipboard,"text",caps,Limits::qemu()).unwrap();
/// let chunks=message.chunks(1024).unwrap();
/// drop(message);
/// drop(chunks);
/// ```
pub struct OwnedMessage
{
    header:  Header,
    encoded: [u8; MESSAGE_HEADER_BYTES],
    payload: Vec<u8>
}
impl OwnedMessage
{
    fn begin(kind: MessageType, length: usize, limits: Limits) -> Result<Self>
    {
        let header = Header::new(kind, length, limits)?;
        let mut payload = Vec::new();
        payload.try_reserve_exact(length).map_err(|_| Error::NoMemory { bytes: length })?;
        Ok(Self { header,
                  encoded: header.encode(),
                  payload })
    }

    fn finish(self, capabilities: Capabilities) -> Result<Self>
    {
        Frame { header:  self.header,
                payload: Some(&self.payload) }.message(capabilities)?;
        Ok(self)
    }

    pub fn announce(capabilities: Capabilities, request_reply: bool, limits: Limits)
                    -> Result<Self>
    {
        let mut value = Self::begin(MessageType::ANNOUNCE_CAPABILITIES, 8, limits)?;
        value.payload.extend_from_slice(&u32::from(request_reply).to_le_bytes());
        value.payload.extend_from_slice(&capabilities.bits().to_le_bytes());
        value.finish(capabilities)
    }

    pub fn grab(selection: Selection, serial: Option<u32>, formats: &[Format],
                capabilities: Capabilities, limits: Limits)
                -> Result<Self>
    {
        let serial_bytes = if capabilities.contains(Capability::ClipboardGrabSerial)
        {
            if serial.is_none()
            {
                return Err(Error::MissingSerial);
            }
            4
        }
        else
        {
            if serial.is_some()
            {
                return Err(Error::UnexpectedSerial);
            }
            0
        };
        let length = formats.len()
                            .checked_mul(4)
                            .and_then(|n| n.checked_add(prefix_bytes(capabilities) + serial_bytes))
                            .ok_or(Error::MessageSize { bytes:   u64::MAX,
                                                        maximum: limits.maximum_message_bytes() })?;
        let mut value = Self::begin(MessageType::CLIPBOARD_GRAB, length, limits)?;
        prefix(&mut value.payload, selection, capabilities)?;
        if let Some(serial) = serial
        {
            value.payload.extend_from_slice(&serial.to_le_bytes());
        }
        for format in formats
        {
            value.payload.extend_from_slice(&format.raw().to_le_bytes());
        }
        value.finish(capabilities)
    }

    pub fn request(selection: Selection, format: Format, capabilities: Capabilities,
                   limits: Limits)
                   -> Result<Self>
    {
        let mut value =
            Self::begin(MessageType::CLIPBOARD_REQUEST, prefix_bytes(capabilities) + 4, limits)?;
        prefix(&mut value.payload, selection, capabilities)?;
        value.payload.extend_from_slice(&format.raw().to_le_bytes());
        value.finish(capabilities)
    }

    pub fn text(selection: Selection, text: &str, capabilities: Capabilities, limits: Limits)
                -> Result<Self>
    {
        Self::data(selection, Format::UTF8_TEXT, text.as_bytes(), capabilities, limits)
    }

    pub fn unavailable(selection: Selection, capabilities: Capabilities, limits: Limits)
                       -> Result<Self>
    {
        Self::data(selection, Format::NONE, &[], capabilities, limits)
    }

    fn data(selection: Selection, format: Format, bytes: &[u8], capabilities: Capabilities,
            limits: Limits)
            -> Result<Self>
    {
        if !capabilities.contains(Capability::ClipboardByDemand)
        {
            return Err(Error::CapabilityRequired(Capability::ClipboardByDemand));
        }
        let length = bytes.len()
                          .checked_add(prefix_bytes(capabilities) + 4)
                          .ok_or(Error::MessageSize { bytes:   u64::MAX,
                                                      maximum: limits.maximum_message_bytes() })?;
        let mut value = Self::begin(MessageType::CLIPBOARD, length, limits)?;
        prefix(&mut value.payload, selection, capabilities)?;
        value.payload.extend_from_slice(&format.raw().to_le_bytes());
        value.payload.extend_from_slice(bytes);
        // The private callers supply either a valid Rust str or NONE + no data.
        // Preserve the type proof instead of scanning outbound UTF-8 again.
        Ok(value)
    }

    pub fn release(selection: Selection, capabilities: Capabilities, limits: Limits)
                   -> Result<Self>
    {
        let mut value =
            Self::begin(MessageType::CLIPBOARD_RELEASE, prefix_bytes(capabilities), limits)?;
        prefix(&mut value.payload, selection, capabilities)?;
        value.finish(capabilities)
    }

    pub fn maximum(maximum: MaximumClipboard, capabilities: Capabilities, limits: Limits)
                   -> Result<Self>
    {
        let mut value = Self::begin(MessageType::MAX_CLIPBOARD, 4, limits)?;
        value.payload.extend_from_slice(&maximum.raw()?.to_le_bytes());
        value.finish(capabilities)
    }

    pub fn header(&self) -> Header
    {
        self.header
    }

    pub fn payload(&self) -> &[u8]
    {
        &self.payload
    }

    pub fn message_bytes(&self) -> usize
    {
        MESSAGE_HEADER_BYTES + self.payload.len()
    }

    pub fn chunks(&self, maximum_chunk_bytes: usize) -> Result<Chunks<'_>>
    {
        Ok(Chunks { message: self,
                    offset:  0,
                    maximum: ChunkPolicy::new(maximum_chunk_bytes)? })
    }
}
fn prefix_bytes(capabilities: Capabilities) -> usize
{
    if capabilities.contains(Capability::ClipboardSelection) { 4 } else { 0 }
}
fn prefix(output: &mut Vec<u8>, selection: Selection, capabilities: Capabilities) -> Result<()>
{
    if capabilities.contains(Capability::ClipboardSelection)
    {
        output.extend_from_slice(&[selection as u8, 0, 0, 0]);
    }
    else if selection != Selection::Clipboard
    {
        return Err(Error::CapabilityRequired(Capability::ClipboardSelection));
    }
    Ok(())
}
pub struct Chunks<'a>
{
    message: &'a OwnedMessage,
    offset:  usize,
    maximum: ChunkPolicy
}
impl<'a> Iterator for Chunks<'a>
{
    type Item = Chunk<'a>;

    fn next(&mut self) -> Option<Self::Item>
    {
        let total = self.message.message_bytes();
        let chunk_header = self.maximum.fragment(total - self.offset)?;
        let end = self.offset + chunk_header.size();
        let header = if self.offset < MESSAGE_HEADER_BYTES
        {
            &self.message.encoded[self.offset..end.min(MESSAGE_HEADER_BYTES)]
        }
        else
        {
            &[]
        };
        let start = self.offset.saturating_sub(MESSAGE_HEADER_BYTES);
        let payload_end = end.saturating_sub(MESSAGE_HEADER_BYTES);
        let payload = &self.message.payload[start..payload_end];
        self.offset = end;
        Some(Chunk { header: chunk_header.encode(),
                     message_header: header,
                     payload })
    }

    fn size_hint(&self) -> (usize, Option<usize>)
    {
        let n = (self.message.message_bytes() - self.offset).div_ceil(self.maximum.maximum());
        (n, Some(n))
    }
}
impl ExactSizeIterator for Chunks<'_>
{
}
impl core::iter::FusedIterator for Chunks<'_>
{
}
pub struct Chunk<'a>
{
    header:         [u8; 8],
    message_header: &'a [u8],
    payload:        &'a [u8]
}
impl Chunk<'_>
{
    pub fn segments(&self) -> [&[u8]; 3]
    {
        [&self.header, self.message_header, self.payload]
    }

    pub fn encoded_bytes(&self) -> usize
    {
        self.header.len() + self.message_header.len() + self.payload.len()
    }
}
