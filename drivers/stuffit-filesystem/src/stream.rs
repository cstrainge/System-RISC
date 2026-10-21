use alloc::vec::Vec;

use mac_driver::{Completion, Error, Phase, Result};
use mac_sit::{Decoder, Fork, MAX_STEP};

use crate::{Sources, buffer, format_error};

/// Owned positional read, retained through cancellation acknowledgement. Short
/// source reads are collected; premature EOF is corruption, never zero fill.
pub(super) struct Fetch
{
    offset:    u64,
    pub bytes: Vec<u8>,
    filled:    usize,
    ticket:    u32,
    forced:    Option<Error>
}
impl Fetch
{
    pub fn new(offset: u64, length: usize) -> Result<Self>
    {
        offset.checked_add(length as u64).ok_or(Error(mac_abi::BAD_ARGUMENT))?;
        Ok(Self { offset,
                  bytes: buffer(length)?,
                  filled: 0,
                  ticket: 0,
                  forced: None })
    }

    pub fn step<S: Sources>(&mut self, sources: &mut S, source: u32, cancel: bool) -> Result<bool>
    {
        let cancelling = cancel || self.forced.is_some();
        if cancelling && self.ticket == 0
        {
            return Err(self.forced.unwrap_or(Error(mac_abi::OPERATION_CANCELLED)));
        }
        if self.filled == self.bytes.len()
        {
            return Ok(true);
        }
        let phase = if self.ticket == 0
        {
            Phase::Start
        }
        else if cancelling
        {
            Phase::Cancel
        }
        else
        {
            Phase::Poll
        };
        let result = sources.read(source,
                                  self.offset + self.filled as u64,
                                  &mut self.ticket,
                                  phase,
                                  &mut self.bytes[self.filled..]);
        match result
        {
            Ok(Completion::Pending) if self.ticket != 0 => Ok(false),
            Ok(Completion::Complete(n)) if self.ticket == 0 =>
            {
                if cancelling
                {
                    return Err(self.forced.unwrap_or(Error(mac_abi::OPERATION_CANCELLED)));
                }
                if n == 0 || n > self.bytes.len() - self.filled
                {
                    return Err(Error(mac_abi::IO_ERROR));
                }
                self.filled += n;
                Ok(self.filled == self.bytes.len())
            }
            Err(error) if self.ticket == 0 => Err(self.forced.unwrap_or(error)),
            result =>
            {
                let error = match result
                {
                    Err(error) => error,
                    _ => Error(mac_abi::BAD_ABI)
                };
                if self.ticket == 0
                {
                    return Err(error);
                }
                // Admission contention is retryable with the exact live ticket.
                if error.0 != mac_abi::SERVICE_BUSY
                {
                    self.forced.get_or_insert(error);
                }
                Ok(false)
            }
        }
    }
}

pub(super) struct Stream
{
    pub fork:     Fork,
    decoder:      Decoder,
    pub position: u64,
    packed:       u64,
    fetch:        Option<Fetch>,
    at:           usize,
    complete:     bool
}
impl Stream
{
    pub fn new(fork: Fork) -> Result<Self>
    {
        Ok(Self { fork,
                  decoder: Decoder::new(fork).map_err(format_error)?,
                  position: 0,
                  packed: 0,
                  fetch: None,
                  at: 0,
                  complete: false })
    }

    /// Advance at most one source callback or one bounded decoder step. End-of-
    /// fork reads don't complete until CRC validation, even if the caller's
    /// requested output buffer ended exactly at the declared fork length.
    pub fn step<S: Sources>(&mut self, sources: &mut S, source: u32, offset: u64,
                            output: &mut [u8], written: &mut usize, cancel: bool)
                            -> Result<bool>
    {
        if cancel
        {
            if let Some(fetch) = &mut self.fetch
               && !fetch.step(sources, source, true)?
            {
                return Ok(false);
            }
            return Err(Error(mac_abi::OPERATION_CANCELLED));
        }
        if self.complete
        {
            return Ok(true);
        }
        let end = offset.checked_add(output.len() as u64).ok_or(Error(mac_abi::BAD_ARGUMENT))?;
        if *written == output.len() && end < self.fork.length
        {
            return Ok(true);
        }
        if let Some(fetch) = &mut self.fetch
           && fetch.filled < fetch.bytes.len()
        {
            return fetch.step(sources, source, false).map(|_| false);
        }
        let input = self.fetch.as_ref().map_or(&[][..], |fetch| &fetch.bytes[self.at..]);
        let mut discard = [0; MAX_STEP];
        let skipping = self.position < offset;
        let target = if skipping
        {
            let count = (offset - self.position).min(MAX_STEP as u64) as usize;
            &mut discard[..count]
        }
        else if *written < output.len()
        {
            &mut output[*written..]
        }
        else
        {
            &mut discard[..1]
        };
        let progress = self.decoder.step(input, target).map_err(format_error)?;
        self.at += progress.consumed;
        self.position += progress.written as u64;
        if !skipping
        {
            *written += progress.written;
        }
        if *written > output.len()
        {
            return Err(Error(mac_abi::IO_ERROR));
        }
        self.complete = progress.complete;
        if self.complete || *written == output.len() && end < self.fork.length
        {
            return Ok(true);
        }
        if progress.consumed == 0 && progress.written == 0
        {
            if self.packed == self.fork.packed_length
            {
                return Err(Error(mac_abi::IO_ERROR));
            }
            let count = (self.fork.packed_length - self.packed).min(MAX_STEP as u64) as usize;
            self.fetch = Some(Fetch::new(self.fork.offset + self.packed, count)?);
            self.packed += count as u64;
            self.at = 0;
            // Yield before calling the lower service. No header/decode work and
            // I/O are hidden in the same unbounded callback.
        }
        Ok(false)
    }
}
