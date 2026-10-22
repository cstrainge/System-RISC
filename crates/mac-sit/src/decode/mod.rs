//! Incremental fork expansion. Input/output ownership remains with the caller;
//! a provider can suspend between source reads without retaining borrowed data.
use crate::{Crc16, Error, Fork, Method, Result};
mod lzw;
mod huffman;
mod arsenic;

/// Bound both input consumption and output production in one scheduling slice.
pub const MAX_STEP: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress
{
    pub consumed:    usize,
    pub written:     usize,
    /// True only after the declared lengths and uncompressed CRC agree.
    pub complete:    bool,
    /// A CPU-only yield needs another step, not another source read.
    pub needs_input: bool
}

pub(super) enum Symbol
{
    Byte(u8),
    Input,
    Yield,
    End
}
pub(super) struct Input<'a>
{
    bytes:     &'a [u8],
    at:        usize,
    remaining: &'a mut u64,
    work:      usize
}
impl Input<'_>
{
    fn work(&mut self) -> bool
    {
        if self.work == 0
        {
            false
        }
        else
        {
            self.work -= 1;
            true
        }
    }

    fn byte(&mut self) -> Option<u8>
    {
        if *self.remaining == 0
        {
            return None;
        }
        let value = *self.bytes.get(self.at)?;
        self.at += 1;
        *self.remaining -= 1;
        Some(value)
    }

    fn exhausted(&self) -> bool
    {
        *self.remaining == 0
    }
}

enum Codec
{
    Stored,
    Rle(Rle),
    Lzw(lzw::Lzw),
    Huffman(huffman::Huffman),
    Arsenic(arsenic::Arsenic)
}

/// A decoder's storage is bounded by the method, never by fork length. Partial
/// output is provisional until `complete`: CRC failure must abort copy-out.
pub struct Decoder
{
    codec:             Codec,
    packed_remaining:  u64,
    remaining:         u64,
    expected_crc:      u16,
    internal_checksum: bool,
    crc:               Crc16,
    failed:            bool,
    complete:          bool
}
impl Decoder
{
    pub fn new(fork: Fork) -> Result<Self>
    {
        let codec = match fork.method()?
        {
            Method::Stored =>
            {
                if fork.length != fork.packed_length
                {
                    return Err(Error::Length);
                }
                Codec::Stored
            }
            Method::Rle => Codec::Rle(Rle::default()),
            Method::Lzw => Codec::Lzw(lzw::Lzw::new()?),
            Method::Huffman => Codec::Huffman(huffman::Huffman::new()?),
            Method::Arsenic => Codec::Arsenic(arsenic::Arsenic::new(fork.length)?)
        };
        Ok(Self { codec,
                  packed_remaining: fork.packed_length,
                  remaining: fork.length,
                  expected_crc: fork.crc,
                  internal_checksum: fork.method()? == Method::Arsenic,
                  crc: Crc16::new(),
                  failed: false,
                  complete: false })
    }

    /// Feed an arbitrary chunk; retain unconsumed input for the next call. A
    /// zero-length output performs no work. Call again after the last output
    /// byte to obtain length/CRC validation. Truncated transport reads must be
    /// reported by the caller; a short chunk is not end-of-fork.
    pub fn step(&mut self, input: &[u8], output: &mut [u8]) -> Result<Progress>
    {
        if self.failed
        {
            return Err(Error::InvalidState);
        }
        let result = self.expand(input, output);
        if result.is_err()
        {
            self.failed = true;
        }
        result
    }

    fn expand(&mut self, bytes: &[u8], output: &mut [u8]) -> Result<Progress>
    {
        let mut input = Input { bytes:     &bytes[..bytes.len().min(MAX_STEP)],
                                at:        0,
                                remaining: &mut self.packed_remaining,
                                work:      MAX_STEP };
        let mut written = 0;
        let mut needs_input = false;
        while !self.complete && written < output.len().min(MAX_STEP)
        {
            let symbol = match &mut self.codec
            {
                Codec::Stored =>
                {
                    match input.byte()
                    {
                        Some(value) => Symbol::Byte(value),
                        None if input.exhausted() => Symbol::End,
                        None => Symbol::Input
                    }
                }
                Codec::Rle(codec) => codec.next(&mut input)?,
                Codec::Lzw(codec) => codec.next(&mut input)?,
                Codec::Huffman(codec) => codec.next(&mut input, self.remaining == 0)?,
                Codec::Arsenic(codec) => codec.next(&mut input)?
            };
            match symbol
            {
                Symbol::Input =>
                {
                    needs_input = true;
                    break;
                }
                Symbol::Yield => break,
                Symbol::End =>
                {
                    if self.remaining != 0
                    {
                        return Err(Error::Length);
                    }
                    if !self.internal_checksum && self.crc.value() != self.expected_crc
                    {
                        return Err(Error::ForkChecksum);
                    }
                    self.complete = true;
                }
                Symbol::Byte(value) =>
                {
                    self.remaining = self.remaining.checked_sub(1).ok_or(Error::Length)?;
                    output[written] = value;
                    if !self.internal_checksum
                    {
                        self.crc.update(&[value]);
                    }
                    written += 1;
                }
            }
        }
        Ok(Progress { consumed: input.at,
                      written,
                      complete: self.complete,
                      needs_input })
    }
}

#[derive(Default)]
struct Rle
{
    previous: Option<u8>,
    escape:   bool,
    repeat:   u8
}
impl Rle
{
    fn next(&mut self, input: &mut Input<'_>) -> Result<Symbol>
    {
        loop
        {
            if self.repeat != 0
            {
                self.repeat -= 1;
                return Ok(Symbol::Byte(self.previous.ok_or(Error::CompressedData)?));
            }
            let Some(value) = input.byte()
            else
            {
                return if !input.exhausted()
                {
                    Ok(Symbol::Input)
                }
                else if self.escape
                {
                    Err(Error::Truncated)
                }
                else
                {
                    Ok(Symbol::End)
                };
            };
            if self.escape
            {
                self.escape = false;
                if value != 0
                {
                    if value == 1 || self.previous.is_none()
                    {
                        return Err(Error::CompressedData);
                    }
                    self.repeat = value - 1;
                    continue;
                }
                self.previous = Some(0x90);
                return Ok(Symbol::Byte(0x90));
            }
            if value == 0x90
            {
                self.escape = true;
            }
            else
            {
                self.previous = Some(value);
                return Ok(Symbol::Byte(value));
            }
        }
    }
}
