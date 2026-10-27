use mac_vdagent_protocol::{ChunkHeader, MESSAGE_HEADER_BYTES, OwnedMessage, Port};

use crate::{Epoch, Error, FetchToken};

pub(crate) const SLOTS: usize = 4;
const CHUNK: usize = 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputToken
{
    pub(crate) epoch: Epoch,
    id:               u64,
    progress:         usize
}
pub struct Output<'a>
{
    pub token:    OutputToken,
    pub segments: [&'a [u8]; 3]
}
impl Output<'_>
{
    pub fn remaining_bytes(&self) -> usize
    {
        self.segments.iter().map(|s| s.len()).sum()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sent
{
    Progress,
    Message,
    Fetch(FetchToken)
}
#[derive(Clone, Copy)]
pub(crate) enum Purpose
{
    Control,
    Guest,
    Response,
    Fetch(FetchToken)
}
pub(crate) struct Transmission
{
    message:        OwnedMessage,
    header:         [u8; MESSAGE_HEADER_BYTES],
    chunk:          [u8; 8],
    message_offset: usize,
    chunk_size:     usize,
    chunk_accepted: usize,
    progress:       usize,
    id:             u64,
    pub purpose:    Purpose
}
impl Transmission
{
    pub fn new(message: OwnedMessage, id: u64, purpose: Purpose) -> Result<Self, Error>
    {
        let chunk_size = CHUNK.min(message.message_bytes());
        Ok(Self { header: message.header().encode(),
                  chunk: ChunkHeader::new(Port::Client, chunk_size)?.encode(),
                  message,
                  message_offset: 0,
                  chunk_size,
                  chunk_accepted: 0,
                  progress: 0,
                  id,
                  purpose })
    }

    pub fn started(&self) -> bool
    {
        self.progress != 0
    }

    pub fn view(&self, epoch: Epoch) -> Output<'_>
    {
        let start = self.message_offset;
        let end = start + self.chunk_size;
        let header = if start < MESSAGE_HEADER_BYTES
        {
            &self.header[start..end.min(MESSAGE_HEADER_BYTES)]
        }
        else
        {
            &[]
        };
        let payload = &self.message.payload()
            [start.saturating_sub(MESSAGE_HEADER_BYTES)..end.saturating_sub(MESSAGE_HEADER_BYTES)];
        let mut skip = self.chunk_accepted;
        let mut segments = [&self.chunk[..], header, payload];
        for segment in &mut segments
        {
            let n = skip.min(segment.len());
            *segment = &segment[n..];
            skip -= n;
        }
        Output { token: OutputToken { epoch,
                                      id: self.id,
                                      progress: self.progress },
                 segments }
    }

    pub fn accepted(&mut self, epoch: Epoch, token: OutputToken, bytes: usize)
                    -> Result<bool, Error>
    {
        if token != self.view(epoch).token || bytes > 8 + self.chunk_size - self.chunk_accepted
        {
            return Err(Error::InvalidProgress);
        }
        self.chunk_accepted += bytes;
        self.progress += bytes;
        if self.chunk_accepted == 8 + self.chunk_size
        {
            self.message_offset += self.chunk_size;
            if self.message_offset == self.message.message_bytes()
            {
                return Ok(true);
            }
            self.chunk_size = CHUNK.min(self.message.message_bytes() - self.message_offset);
            self.chunk = ChunkHeader::new(Port::Client, self.chunk_size)?.encode();
            self.chunk_accepted = 0;
        }
        Ok(false)
    }
}
pub(crate) struct Queue
{
    slots: [Option<Transmission>; SLOTS],
    len:   usize
}
impl Queue
{
    pub fn new() -> Self
    {
        Self { slots: core::array::from_fn(|_| None),
               len:   0 }
    }

    pub fn len(&self) -> usize
    {
        self.len
    }

    pub fn front(&self) -> Option<&Transmission>
    {
        self.slots[0].as_ref()
    }

    pub fn front_mut(&mut self) -> Option<&mut Transmission>
    {
        self.slots[0].as_mut()
    }

    pub fn remove(&mut self, index: usize)
    {
        for i in index..self.len - 1
        {
            self.slots[i] = self.slots[i + 1].take();
        }
        self.len -= 1;
        self.slots[self.len] = None;
    }

    pub fn push(&mut self, value: Transmission) -> Result<(), Error>
    {
        if self.len == SLOTS
        {
            return Err(Error::QueueFull);
        }
        self.slots[self.len] = Some(value);
        self.len += 1;
        Ok(())
    }

    pub fn response_pending(&self) -> bool
    {
        self.slots[..self.len].iter().flatten().any(|tx| matches!(tx.purpose, Purpose::Response))
    }

    pub fn room_after_guest_cancel(&self) -> Result<bool, Error>
    {
        if self.front().is_some_and(|tx| {
                           tx.started() && matches!(tx.purpose, Purpose::Guest | Purpose::Response)
                       })
        {
            return Err(Error::ResetRequired);
        }
        Ok(self.slots[..self.len].iter()
                                 .flatten()
                                 .filter(|tx| {
                                     tx.started()
                                     || !matches!(tx.purpose, Purpose::Guest | Purpose::Response)
                                 })
                                 .count()
           < SLOTS)
    }

    pub fn cancel_guest(&mut self) -> Result<(), Error>
    {
        if self.front().is_some_and(|tx| {
                           tx.started() && matches!(tx.purpose, Purpose::Guest | Purpose::Response)
                       })
        {
            return Err(Error::ResetRequired);
        }
        let mut index = 0;
        while index < self.len
        {
            if self.slots[index].as_ref().is_some_and(|tx| {
                                             !tx.started()
                                             && matches!(tx.purpose,
                                                         Purpose::Guest | Purpose::Response)
                                         })
            {
                self.remove(index);
            }
            else
            {
                index += 1;
            }
        }
        Ok(())
    }
}
