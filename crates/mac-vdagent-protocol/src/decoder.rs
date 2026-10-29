use alloc::vec::Vec;

use crate::{CHUNK_HEADER_BYTES, ChunkHeader, Error, Frame, Header, Limits, MESSAGE_HEADER_BYTES,
            Port};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IgnoredChunk
{
    pub port:  Port,
    pub bytes: usize
}
#[derive(Debug)]
pub struct Feed<'a>
{
    pub consumed: usize,
    pub frame:    Option<Frame<'a>>,
    pub ignored:  Option<IgnoredChunk>
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedError
{
    pub consumed: usize,
    pub cause:    Error
}
impl core::fmt::Display for FeedError
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        self.cause.fmt(f)
    }
}
impl core::error::Error for FeedError
{
}

/// Owns receive storage across every fragment. Callers may change input buffers
/// freely; no borrowed scratch is used as partially assembled message storage.
/// A returned frame borrows this decoder until the caller finishes consuming it.
///
/// ```compile_fail
/// use mac_vdagent_protocol::{Decoder,Limits};
/// fn overlap(decoder:&mut Decoder,input:&[u8]) {
///     let first=decoder.feed(input).unwrap();
///     decoder.feed(input).unwrap();
///     drop(first);
/// }
/// ```
pub struct Decoder
{
    limits:          Limits,
    chunk_raw:       [u8; CHUNK_HEADER_BYTES],
    chunk_filled:    usize,
    chunk:           Option<ChunkHeader>,
    chunk_remaining: usize,
    message_raw:     [u8; MESSAGE_HEADER_BYTES],
    message_filled:  usize,
    header:          Option<Header>,
    body_seen:       usize,
    payload:         Vec<u8>,
    ready:           bool,
    poisoned:        bool
}
impl Decoder
{
    pub fn new(limits: Limits) -> Self
    {
        Self { limits,
               chunk_raw: [0; CHUNK_HEADER_BYTES],
               chunk_filled: 0,
               chunk: None,
               chunk_remaining: 0,
               message_raw: [0; MESSAGE_HEADER_BYTES],
               message_filled: 0,
               header: None,
               body_seen: 0,
               payload: Vec::new(),
               ready: false,
               poisoned: false }
    }

    pub fn is_poisoned(&self) -> bool
    {
        self.poisoned
    }

    pub fn retained_capacity(&self) -> usize
    {
        self.payload.capacity()
    }

    /// Retry a fully decoded frame without feeding later bytes past a refused
    /// consumer effect. Its payload remains borrowed from this exact decoder.
    pub fn current_frame(&self) -> Option<Frame<'_>>
    {
        if !self.ready
        {
            return None;
        }
        let header = self.header?;
        Some(Frame { header,
                     payload: if header.kind().retained() { Some(&self.payload) } else { None } })
    }

    /// Validate a closed transport before discarding its decoder. Partial wire
    /// state is a truncation, never a complete message or successful transfer.
    pub fn end_of_stream(&self) -> crate::Result<()>
    {
        if self.poisoned
        {
            return Err(Error::Poisoned);
        }
        if self.chunk_filled != 0
           || self.chunk_remaining != 0
           || (!self.ready && (self.message_filled != 0 || self.header.is_some()))
        {
            return Err(Error::Length);
        }
        Ok(())
    }

    /// Returns at most one frame/ignored chunk. Resubmit `input[consumed..]` for
    /// the next event. A NoMemory refusal keeps the parsed header and exact input
    /// position; it is retryable. Other framing errors poison the stream, which
    /// must be replaced only after the transport/connection has been reset.
    pub fn feed<'a>(&'a mut self, input: &[u8]) -> core::result::Result<Feed<'a>, FeedError>
    {
        if self.poisoned
        {
            return Err(FeedError { consumed: 0,
                                   cause:    Error::Poisoned });
        }
        if self.ready
        {
            self.header = None;
            self.message_filled = 0;
            self.body_seen = 0;
            self.payload.clear();
            self.ready = false;
        }
        let mut consumed = 0;
        loop
        {
            if let Some(header) = self.header
            {
                if header.kind().retained() && self.payload.capacity() < header.payload_bytes()
                {
                    if self.payload
                           .try_reserve_exact(header.payload_bytes() - self.payload.len())
                           .is_err()
                    {
                        return Err(FeedError { consumed,
                                               cause: Error::NoMemory { bytes:
                                                                            header.payload_bytes() } });
                    }
                }
                if self.body_seen == header.payload_bytes()
                {
                    self.ready = true;
                    let payload =
                        if header.kind().retained() { Some(&self.payload[..]) } else { None };
                    return Ok(Feed { consumed,
                                     frame: Some(Frame { header,
                                                         payload }),
                                     ignored: None });
                }
            }
            if consumed == input.len()
            {
                return Ok(Feed { consumed,
                                 frame: None,
                                 ignored: None });
            }
            if self.chunk_remaining == 0
            {
                self.chunk = None;
                let count = (CHUNK_HEADER_BYTES - self.chunk_filled).min(input.len() - consumed);
                self.chunk_raw[self.chunk_filled..self.chunk_filled+count].copy_from_slice(&input[consumed..consumed+count]);
                self.chunk_filled += count;
                consumed += count;
                if self.chunk_filled != CHUNK_HEADER_BYTES
                {
                    return Ok(Feed { consumed,
                                     frame: None,
                                     ignored: None });
                }
                let header = match ChunkHeader::decode(&self.chunk_raw)
                {
                    Ok(header) => header,
                    Err(cause) =>
                    {
                        self.poisoned = true;
                        return Err(FeedError { consumed,
                                               cause });
                    }
                };
                self.chunk_filled = 0;
                self.chunk_remaining = header.size();
                self.chunk = Some(header);
            }
            let Some(chunk) = self.chunk
            else
            {
                self.poisoned = true;
                return Err(FeedError { consumed,
                                       cause: Error::Poisoned });
            };
            if chunk.port() != Port::Client
            {
                let count = self.chunk_remaining.min(input.len() - consumed);
                consumed += count;
                self.chunk_remaining -= count;
                if self.chunk_remaining == 0
                {
                    return Ok(Feed { consumed,
                                     frame: None,
                                     ignored: Some(IgnoredChunk { port:  chunk.port(),
                                                                  bytes: chunk.size() }) });
                }
                continue;
            }
            if self.header.is_none()
            {
                let count =
                    (MESSAGE_HEADER_BYTES - self.message_filled).min(self.chunk_remaining)
                                                                .min(input.len() - consumed);
                self.message_raw[self.message_filled..self.message_filled+count].copy_from_slice(&input[consumed..consumed+count]);
                self.message_filled += count;
                consumed += count;
                self.chunk_remaining -= count;
                if self.message_filled == MESSAGE_HEADER_BYTES
                {
                    match Header::decode(&self.message_raw, self.limits)
                    {
                        Ok(header) =>
                        {
                            self.header = Some(header);
                            self.payload.clear();
                            self.body_seen = 0;
                        }
                        Err(cause) =>
                        {
                            self.poisoned = true;
                            return Err(FeedError { consumed,
                                                   cause });
                        }
                    }
                }
                continue;
            }
            let Some(header) = self.header
            else
            {
                self.poisoned = true;
                return Err(FeedError { consumed,
                                       cause: Error::Poisoned });
            };
            let count = (header.payload_bytes() - self.body_seen).min(self.chunk_remaining)
                                                                 .min(input.len() - consumed);
            if header.kind().retained()
            {
                self.payload.extend_from_slice(&input[consumed..consumed + count]);
            }
            self.body_seen += count;
            consumed += count;
            self.chunk_remaining -= count;
        }
    }
}
