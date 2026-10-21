//! StuffIt method 2: raw compress block mode, maximum 14-bit codes. Codes are
//! little-endian within groups of eight. Width changes and CLEAR discard the
//! rest of the old group; this is not the GIF/TIFF LZW bitstream.
use alloc::vec::Vec;

use super::{Input, Symbol};
use crate::{Error, Result};

const CAPACITY: usize = 1 << 14;
#[derive(Clone, Copy, Default)]
struct Entry
{
    prefix: u16,
    suffix: u8
}
pub(super) struct Lzw
{
    dictionary: Vec<Entry>,
    stack:      Vec<u8>,
    next:       usize,
    previous:   Option<u16>,
    first:      u8,
    width:      usize,
    group:      [u8; 14],
    filled:     usize,
    bit:        usize,
    ready:      bool
}
impl Lzw
{
    pub(super) fn new() -> Result<Self>
    {
        let mut dictionary = Vec::new();
        dictionary.try_reserve_exact(CAPACITY).map_err(|_| Error::Allocation)?;
        dictionary.resize(CAPACITY, Entry::default());
        let mut stack = Vec::new();
        stack.try_reserve_exact(CAPACITY).map_err(|_| Error::Allocation)?;
        Ok(Self { dictionary,
                  stack,
                  next: 257,
                  previous: None,
                  first: 0,
                  width: 9,
                  group: [0; 14],
                  filled: 0,
                  bit: 0,
                  ready: false })
    }

    fn discard_group(&mut self)
    {
        self.filled = 0;
        self.bit = 0;
        self.ready = false;
    }

    pub(super) fn next(&mut self, input: &mut Input<'_>) -> Result<Symbol>
    {
        loop
        {
            if let Some(byte) = self.stack.pop()
            {
                return Ok(Symbol::Byte(byte));
            }
            if self.width < 14 && self.next >= 1 << self.width
            {
                self.width += 1;
                self.discard_group();
            }
            if !self.ready
            {
                while self.filled < self.width
                {
                    match input.byte()
                    {
                        Some(value) =>
                        {
                            self.group[self.filled] = value;
                            self.filled += 1;
                        }
                        None if input.exhausted() => break,
                        None => return Ok(Symbol::Input)
                    }
                }
                self.ready = true;
            }
            if self.bit + self.width > self.filled * 8
            {
                if input.exhausted()
                {
                    return Ok(Symbol::End);
                }
                self.discard_group();
                continue;
            }
            let mut code = 0u16;
            for bit in 0..self.width
            {
                let at = self.bit + bit;
                code |= u16::from((self.group[at / 8] >> (at % 8)) & 1) << bit;
            }
            self.bit += self.width;
            if code == 256
            {
                self.next = 257;
                self.previous = None;
                self.width = 9;
                self.discard_group();
                continue;
            }
            let original = code;
            if usize::from(code) == self.next
            {
                self.stack.push(self.first);
                code = self.previous.ok_or(Error::CompressedData)?;
            }
            else if usize::from(code) > self.next
            {
                return Err(Error::CompressedData);
            }
            // Each dictionary edge points to an earlier code, bounding both
            // traversal and stack length without a cycle/recursion hazard.
            while code >= 257
            {
                let entry = *self.dictionary.get(usize::from(code)).ok_or(Error::CompressedData)?;
                if entry.prefix >= code
                {
                    return Err(Error::CompressedData);
                }
                self.stack.push(entry.suffix);
                code = entry.prefix;
            }
            if code >= 256
            {
                return Err(Error::CompressedData);
            }
            self.first = code as u8;
            self.stack.push(self.first);
            if let Some(previous) = self.previous
               && self.next < CAPACITY
            {
                self.dictionary[self.next] = Entry { prefix: previous,
                                                     suffix: self.first };
                self.next += 1;
            }
            self.previous = Some(original);
        }
    }
}
