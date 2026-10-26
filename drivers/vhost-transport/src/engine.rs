//! Bounded progress: each callback transfers at most two 4 KiB fragments in
//! each direction. Nothing borrowed from a caller survives a callback.
#![forbid(unsafe_code)]
use mac_abi::{BAD_ABI, BAD_ARGUMENT, IO_ERROR, NOT_FOUND, SERVICE_BUSY};
use mac_driver::{Completion, Error, Phase, Request, Result};
use mac_host_protocol::{self as wire, Arguments, Operation, Receiver};
pub const FRAGMENT: usize = 4096;
pub const MAX_PACKET: usize = wire::HEADER_BYTES + wire::MAX_PAYLOAD;
pub const STALL_MICROSECONDS: u64 = 5_000_000;
pub trait Channel
{
    fn monotonic_microseconds(&self) -> u64;
    /// Progress control notifications and reclaim bounded TX completions.
    /// False means initial connection is not ready; a lost established
    /// connection is an error and poisons this incarnation.
    fn poll(&mut self) -> Result<bool>;
    fn send(&mut self, bytes: &[u8]) -> Result<usize>;
    fn receive(&mut self, bytes: &mut [u8]) -> Result<usize>;
}
pub struct Buffers
{
    tx:      [u8; MAX_PACKET],
    payload: [u8; wire::MAX_PAYLOAD]
}
impl Buffers
{
    pub const fn new() -> Self
    {
        Self { tx:      [0; MAX_PACKET],
               payload: [0; wire::MAX_PAYLOAD] }
    }
}
impl Default for Buffers
{
    fn default() -> Self
    {
        Self::new()
    }
}
pub struct Engine
{
    tx_len:        usize,
    tx_sent:       usize,
    ticket:        u64,
    operation:     u32,
    receiver:      Option<Receiver>,
    poisoned:      bool,
    last_progress: u64
}
impl Default for Engine
{
    fn default() -> Self
    {
        Self::new()
    }
}
impl Engine
{
    pub const fn new() -> Self
    {
        Self { tx_len:        0,
               tx_sent:       0,
               ticket:        0,
               operation:     0,
               receiver:      None,
               poisoned:      false,
               last_progress: 0 }
    }

    pub fn request(&mut self, channel: &mut impl Channel, request: Request<'_>, output: &mut [u8],
                   buffers: &mut Buffers)
                   -> Result<Completion>
    {
        let result = self.request_inner(channel, request, output, buffers);
        if result.is_err() && self.ticket == request.ticket
        {
            self.poisoned = true;
            self.ticket = 0;
            self.receiver = None;
        }
        result
    }

    fn request_inner(&mut self, channel: &mut impl Channel, request: Request<'_>,
                     output: &mut [u8], buffers: &mut Buffers)
                     -> Result<Completion>
    {
        if self.poisoned
        {
            return Err(Error(IO_ERROR));
        }
        if request.class.0 != wire::DIRECTORY_CLASS
           || request.ticket == 0
           || output.len() < wire::HEADER_BYTES
        {
            return Err(Error(BAD_ARGUMENT));
        }
        if request.phase == Phase::Start
        {
            if self.ticket != 0
            {
                return Err(Error(SERVICE_BUSY));
            }
            let arguments = Arguments::decode(request.input).map_err(|_| Error(BAD_ARGUMENT))?;
            let operation =
                Operation::try_from(u16::try_from(request.operation).map_err(|_| {
                                                                        Error(BAD_ARGUMENT)
                                                                    })?).map_err(|_| {
                                                                            Error(BAD_ARGUMENT)
                                                                        })?;
            let header =
                arguments.header(operation, request.ticket).map_err(|_| Error(BAD_ARGUMENT))?;
            match channel.poll()
            {
                Ok(true) => (),
                Ok(false) => return Err(Error(SERVICE_BUSY)),
                Err(error) =>
                {
                    self.poisoned = true;
                    return Err(error);
                }
            }
            buffers.tx[..wire::HEADER_BYTES].copy_from_slice(&header.encode().map_err(|_| {
                                                                               Error(BAD_ARGUMENT)
                                                                           })?);
            buffers.tx[wire::HEADER_BYTES..wire::HEADER_BYTES+arguments.payload.len()].copy_from_slice(arguments.payload);
            self.tx_len = wire::HEADER_BYTES + arguments.payload.len();
            self.tx_sent = 0;
            self.ticket = request.ticket;
            self.last_progress = channel.monotonic_microseconds();
            self.operation = request.operation;
            self.receiver = Some(Receiver::new(request.ticket, operation));
        }
        else
        {
            if self.ticket != request.ticket
               || self.operation != request.operation
               || !request.input.is_empty()
            {
                return Err(Error(NOT_FOUND));
            }
            // A virtconsole peer can disappear without a PORT_OPEN edge.
            // Bound every admitted packet by progress, including cancellation.
            // Never reuse this incarnation after an unknown partial reply.
            if channel.monotonic_microseconds().saturating_sub(self.last_progress)
               >= STALL_MICROSECONDS
            {
                return Err(Error(IO_ERROR));
            }
            if !channel.poll()?
            {
                return Ok(Completion::Pending);
            }
        }
        let result = self.progress(channel, output, buffers);
        if result.is_err()
        {
            self.poisoned = true;
            self.ticket = 0;
            self.receiver = None;
        }
        result
    }

    fn progress(&mut self, channel: &mut impl Channel, output: &mut [u8], buffers: &mut Buffers)
                -> Result<Completion>
    {
        for _ in 0..2
        {
            if self.tx_sent == self.tx_len
            {
                break;
            }
            let end = (self.tx_sent + FRAGMENT).min(self.tx_len);
            let count = channel.send(&buffers.tx[self.tx_sent..end])?;
            if count > end - self.tx_sent
            {
                return Err(Error(BAD_ABI));
            }
            self.tx_sent += count;
            if count != 0
            {
                self.last_progress = channel.monotonic_microseconds();
            }
            if count == 0
            {
                break;
            }
        }
        let mut fragment = [0; FRAGMENT];
        for _ in 0..2
        {
            let count = channel.receive(&mut fragment)?;
            if count == 0
            {
                break;
            }
            let bytes = fragment.get(..count).ok_or(Error(BAD_ABI))?;
            self.last_progress = channel.monotonic_microseconds();
            let receiver = self.receiver.as_mut().ok_or(Error(BAD_ABI))?;
            if receiver.feed(bytes, &mut buffers.payload).map_err(|_| Error(BAD_ABI))? != count
            {
                return Err(Error(BAD_ABI));
            }
            if let Some(header) = receiver.completed()
            {
                if self.tx_sent != self.tx_len
                {
                    return Err(Error(BAD_ABI));
                }
                let length = wire::HEADER_BYTES + header.payload as usize;
                let target = output.get_mut(..length).ok_or(Error(BAD_ARGUMENT))?;
                target[..wire::HEADER_BYTES].copy_from_slice(&header.encode()
                                                                    .map_err(|_| Error(BAD_ABI))?);
                target[wire::HEADER_BYTES..].copy_from_slice(&buffers.payload
                                                                 [..header.payload as usize]);
                self.ticket = 0;
                self.receiver = None;
                return Ok(Completion::Complete(length));
            }
        }
        Ok(Completion::Pending)
    }
}
