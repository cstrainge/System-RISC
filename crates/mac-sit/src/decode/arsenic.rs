//! StuffIt method 15: arithmetic coding, MTF, inverse BWT and byte RLE.
//! Every phase is resumable at an arbitrary source chunk boundary. Table
//! construction also yields; no callback expands a whole block at once.
use alloc::vec::Vec;

use super::{Input, Symbol};
use crate::{Error, Result};
#[path = "arsenic_table.rs"]
mod table;
use table::RANDOMIZATION_TABLE;
const ONE: u32 = 1 << 25;
const HALF: u32 = 1 << 24;
struct Model
{
    first:       u16,
    count:       usize,
    increment:   u16,
    limit:       u32,
    total:       u32,
    frequencies: [u16; 128]
}
impl Model
{
    fn new(first: u16, count: usize, increment: u16, limit: u32) -> Self
    {
        let mut model = Self { first,
                               count,
                               increment,
                               limit,
                               total: 0,
                               frequencies: [0; 128] };
        model.reset();
        model
    }

    fn reset(&mut self)
    {
        self.frequencies[..self.count].fill(self.increment);
        self.total = self.count as u32 * u32::from(self.increment);
    }

    fn update(&mut self, index: usize)
    {
        self.frequencies[index] += self.increment;
        self.total += u32::from(self.increment);
        if self.total > self.limit
        {
            self.total = 0;
            for f in &mut self.frequencies[..self.count]
            {
                *f = (*f + 1) >> 1;
                self.total += u32::from(*f);
            }
        }
    }
}
#[derive(Default)]
struct Bits
{
    value: u64,
    count: u8
}
impl Bits
{
    fn read(&mut self, count: u8, input: &mut Input<'_>) -> Result<Option<u32>>
    {
        while self.count < count
        {
            let Some(byte) = input.byte()
            else
            {
                return if input.exhausted() { Err(Error::Truncated) } else { Ok(None) };
            };
            self.value = (self.value << 8) | u64::from(byte);
            self.count += 8;
        }
        self.count -= count;
        let value = ((self.value >> self.count) & ((1u64 << count) - 1)) as u32;
        self.value &= (1u64 << self.count) - 1;
        Ok(Some(value))
    }
}
struct Arithmetic
{
    range:   u32,
    code:    u32,
    pending: Option<u16>,
    bits:    Bits
}
impl Arithmetic
{
    fn new() -> Self
    {
        Self { range:   ONE,
               code:    0,
               pending: None,
               bits:    Bits::default() }
    }

    fn initialize(&mut self, input: &mut Input<'_>) -> Result<bool>
    {
        let Some(code) = self.bits.read(26, input)?
        else
        {
            return Ok(false);
        };
        if code >= ONE
        {
            return Err(Error::CompressedData);
        }
        self.code = code;
        Ok(true)
    }

    fn symbol(&mut self, model: &mut Model, input: &mut Input<'_>) -> Result<Option<u16>>
    {
        if self.pending.is_none()
        {
            let factor = self.range / model.total;
            if factor == 0 || self.code >= self.range
            {
                return Err(Error::CompressedData);
            }
            let frequency = self.code / factor;
            let mut cumulative = 0;
            let mut index = 0;
            while index + 1 < model.count
                  && cumulative + u32::from(model.frequencies[index]) <= frequency
            {
                cumulative += u32::from(model.frequencies[index]);
                index += 1;
            }
            let size = u32::from(model.frequencies[index]);
            let low = factor * cumulative;
            self.code = self.code.checked_sub(low).ok_or(Error::CompressedData)?;
            self.range =
                if cumulative + size == model.total { self.range - low } else { factor * size };
            self.pending = Some(model.first + index as u16);
            model.update(index);
        }
        while self.range <= HALF
        {
            let Some(bit) = self.bits.read(1, input)?
            else
            {
                return Ok(None);
            };
            self.range <<= 1;
            self.code = (self.code << 1) | bit;
        }
        Ok(self.pending.take())
    }
}
#[derive(Default)]
struct Number
{
    value: u32,
    count: u8
}
impl Number
{
    fn read(&mut self, bits: u8, decoder: &mut Arithmetic, model: &mut Model,
            input: &mut Input<'_>)
            -> Result<Option<u32>>
    {
        while self.count < bits
        {
            let Some(bit) = decoder.symbol(model, input)?
            else
            {
                return Ok(None);
            };
            self.value |= u32::from(bit) << self.count;
            self.count += 1;
        }
        let value = self.value;
        *self = Self::default();
        Ok(Some(value))
    }
}
#[derive(Clone, Copy)]
enum Stage
{
    Initialize,
    SignatureA,
    SignatureS,
    BlockBits,
    BlockStart,
    Randomized,
    Index,
    Selector,
    ZeroAdd(u16),
    ZeroNext,
    FillRun,
    MoveToFront,
    Histogram,
    Prefix,
    ZeroTransform,
    Transform,
    Output,
    Checksum,
    Done
}
struct State
{
    stage:            Stage,
    decoder:          Arithmetic,
    number:           Number,
    initial:          Model,
    selector:         Model,
    models:           [Model; 7],
    mtf:              [u8; 256],
    block:            Vec<u8>,
    transform:        Vec<u32>,
    block_bits:       u8,
    block_size:       usize,
    fork_length:      u64,
    allocation_limit: usize,
    index:            usize,
    at:               usize,
    counts:           [usize; 256],
    selected:         u16,
    zero_state:       usize,
    zero_count:       usize,
    randomized:       bool,
    rand_index:       usize,
    rand_count:       usize,
    last:             u8,
    equal:            u8,
    repeat:           u8,
    crc:              u32
}
impl State
{
    pub fn new(fork_length: u64) -> Self
    {
        Self { stage: Stage::Initialize,
               decoder: Arithmetic::new(),
               number: Number::default(),
               initial: Model::new(0, 2, 1, 256),
               selector: Model::new(0, 11, 8, 1024),
               models: [Model::new(2, 2, 8, 1024),
                        Model::new(4, 4, 4, 1024),
                        Model::new(8, 8, 4, 1024),
                        Model::new(16, 16, 4, 1024),
                        Model::new(32, 32, 2, 1024),
                        Model::new(64, 64, 2, 1024),
                        Model::new(128, 128, 1, 1024)],
               mtf: [0; 256],
               block: Vec::new(),
               transform: Vec::new(),
               block_bits: 0,
               block_size: 0,
               fork_length,
               allocation_limit: 0,
               index: 0,
               at: 0,
               counts: [0; 256],
               selected: 0,
               zero_state: 0,
               zero_count: 0,
               randomized: false,
               rand_index: 0,
               rand_count: 0,
               last: 0,
               equal: 0,
               repeat: 0,
               crc: u32::MAX }
    }

    fn number(&mut self, bits: u8, input: &mut Input<'_>) -> Result<Option<u32>>
    {
        self.number.read(bits, &mut self.decoder, &mut self.initial, input)
    }

    fn append(&mut self, byte: u8) -> Result<()>
    {
        if self.block.len() == self.block_size
        {
            return Err(Error::CompressedData);
        }
        if self.block.len() == self.allocation_limit
        {
            return Err(Error::Length);
        }
        self.block.push(byte);
        Ok(())
    }

    fn select(&mut self, selector: u16) -> Result<()>
    {
        if selector == 10
        {
            if self.block.is_empty() || self.index >= self.block.len()
            {
                return Err(Error::CompressedData);
            }
            self.selector.reset();
            for m in &mut self.models
            {
                m.reset();
            }
            self.transform.clear();
            self.transform.try_reserve_exact(self.block.len()).map_err(|_| Error::Allocation)?;
            self.counts.fill(0);
            self.at = 0;
            self.stage = Stage::Histogram;
        }
        else if selector == 2
        {
            self.selected = 1;
            self.stage = Stage::MoveToFront;
        }
        else if (3..=9).contains(&selector)
        {
            self.selected = selector;
            self.stage = Stage::MoveToFront;
        }
        else
        {
            return Err(Error::CompressedData);
        }
        Ok(())
    }

    fn emit(&mut self, byte: u8) -> Symbol
    {
        self.crc = super::super::crc::update_crc32(self.crc, byte);
        Symbol::Byte(byte)
    }

    pub fn next(&mut self, input: &mut Input<'_>) -> Result<Symbol>
    {
        loop
        {
            if !input.work()
            {
                return Ok(Symbol::Yield);
            }
            match self.stage
            {
                Stage::Initialize =>
                {
                    if !self.decoder.initialize(input)?
                    {
                        return Ok(Symbol::Input);
                    }
                    self.stage = Stage::SignatureA;
                }
                Stage::SignatureA | Stage::SignatureS =>
                {
                    let Some(byte) = self.number(8, input)?
                    else
                    {
                        return Ok(Symbol::Input);
                    };
                    let a = matches!(self.stage, Stage::SignatureA);
                    if byte != u32::from(if a { b'A' } else { b's' })
                    {
                        return Err(Error::CompressedData);
                    }
                    self.stage = if a { Stage::SignatureS } else { Stage::BlockBits };
                }
                Stage::BlockBits =>
                {
                    let Some(bits) = self.number(4, input)?
                    else
                    {
                        return Ok(Symbol::Input);
                    };
                    self.block_bits = bits as u8 + 9;
                    self.block_size = 1usize << self.block_bits;
                    // RLE adds at most one count per four output bytes. Tiny
                    // resource forks must not reserve a whole advertised block.
                    self.allocation_limit = self.fork_length
                                                .saturating_add(self.fork_length / 4)
                                                .min(self.block_size as u64)
                                            as usize;
                    self.block
                        .try_reserve_exact(self.allocation_limit)
                        .map_err(|_| Error::Allocation)?;
                    self.stage = Stage::BlockStart;
                }
                Stage::BlockStart =>
                {
                    let Some(end) = self.decoder.symbol(&mut self.initial, input)?
                    else
                    {
                        return Ok(Symbol::Input);
                    };
                    if end != 0
                    {
                        self.stage = Stage::Checksum;
                        continue;
                    }
                    for (i, slot) in self.mtf.iter_mut().enumerate()
                    {
                        *slot = i as u8;
                    }
                    self.block.clear();
                    self.stage = Stage::Randomized;
                }
                Stage::Randomized =>
                {
                    let Some(value) = self.decoder.symbol(&mut self.initial, input)?
                    else
                    {
                        return Ok(Symbol::Input);
                    };
                    self.randomized = value != 0;
                    self.stage = Stage::Index;
                }
                Stage::Index =>
                {
                    let Some(index) = self.number(self.block_bits, input)?
                    else
                    {
                        return Ok(Symbol::Input);
                    };
                    self.index = index as usize;
                    self.stage = Stage::Selector;
                }
                Stage::Selector =>
                {
                    let Some(selector) = self.decoder.symbol(&mut self.selector, input)?
                    else
                    {
                        return Ok(Symbol::Input);
                    };
                    if selector < 2
                    {
                        self.zero_state = 1;
                        self.zero_count = 0;
                        self.stage = Stage::ZeroAdd(selector);
                    }
                    else
                    {
                        self.select(selector)?;
                    }
                }
                Stage::ZeroAdd(selector) =>
                {
                    self.zero_count = self.zero_count
                                          .checked_add(self.zero_state
                                                           .checked_mul(usize::from(selector) + 1)
                                                           .ok_or(Error::CompressedData)?)
                                          .ok_or(Error::CompressedData)?;
                    if self.zero_count > self.block_size - self.block.len()
                    {
                        return Err(Error::CompressedData);
                    }
                    self.zero_state =
                        self.zero_state.checked_mul(2).ok_or(Error::CompressedData)?;
                    self.stage = Stage::ZeroNext;
                }
                Stage::ZeroNext =>
                {
                    let Some(selector) = self.decoder.symbol(&mut self.selector, input)?
                    else
                    {
                        return Ok(Symbol::Input);
                    };
                    if selector < 2
                    {
                        self.stage = Stage::ZeroAdd(selector);
                    }
                    else
                    {
                        self.selected = selector;
                        self.stage = Stage::FillRun;
                    }
                }
                Stage::FillRun =>
                {
                    if self.zero_count != 0
                    {
                        self.append(self.mtf[0])?;
                        self.zero_count -= 1;
                    }
                    else
                    {
                        self.select(self.selected)?;
                    }
                }
                Stage::MoveToFront =>
                {
                    let symbol = if self.selected == 1
                    {
                        1
                    }
                    else
                    {
                        let Some(symbol) =
                            self.decoder
                                .symbol(&mut self.models[self.selected as usize - 3], input)?
                        else
                        {
                            return Ok(Symbol::Input);
                        };
                        symbol as usize
                    };
                    let byte = self.mtf[symbol];
                    self.mtf.copy_within(0..symbol, 1);
                    self.mtf[0] = byte;
                    self.append(byte)?;
                    self.stage = Stage::Selector;
                }
                Stage::Histogram =>
                {
                    if self.at < self.block.len()
                    {
                        self.counts[self.block[self.at] as usize] += 1;
                        self.at += 1;
                    }
                    else
                    {
                        self.stage = Stage::Prefix;
                    }
                }
                Stage::Prefix =>
                {
                    let mut sum = 0;
                    for count in &mut self.counts
                    {
                        let size = *count;
                        *count = sum;
                        sum += size;
                    }
                    self.stage = Stage::ZeroTransform;
                }
                Stage::ZeroTransform =>
                {
                    if self.transform.len() < self.block.len()
                    {
                        self.transform.push(0);
                    }
                    else
                    {
                        self.at = 0;
                        self.stage = Stage::Transform;
                    }
                }
                Stage::Transform =>
                {
                    if self.at < self.block.len()
                    {
                        let byte = self.block[self.at] as usize;
                        self.transform[self.counts[byte]] = self.at as u32;
                        self.counts[byte] += 1;
                        self.at += 1;
                    }
                    else
                    {
                        self.at = 0;
                        self.equal = 0;
                        self.repeat = 0;
                        self.last = 0;
                        self.rand_index = 0;
                        self.rand_count = RANDOMIZATION_TABLE[0] as usize;
                        self.stage = Stage::Output;
                    }
                }
                Stage::Output =>
                {
                    if self.repeat != 0
                    {
                        self.repeat -= 1;
                        return Ok(self.emit(self.last));
                    }
                    if self.at == self.block.len()
                    {
                        self.stage = Stage::BlockStart;
                        continue;
                    }
                    self.index = self.transform[self.index] as usize;
                    let mut byte = self.block[self.index];
                    if self.randomized && self.rand_count == self.at
                    {
                        byte ^= 1;
                        self.rand_index = (self.rand_index + 1) & 255;
                        self.rand_count += RANDOMIZATION_TABLE[self.rand_index] as usize;
                    }
                    self.at += 1;
                    if self.equal == 4
                    {
                        self.equal = 0;
                        if byte == 0
                        {
                            continue;
                        }
                        self.repeat = byte - 1;
                        return Ok(self.emit(self.last));
                    }
                    if byte == self.last
                    {
                        self.equal += 1;
                    }
                    else
                    {
                        self.last = byte;
                        self.equal = 1;
                    }
                    return Ok(self.emit(byte));
                }
                Stage::Checksum =>
                {
                    let Some(expected) = self.number(32, input)?
                    else
                    {
                        return Ok(Symbol::Input);
                    };
                    if expected != !self.crc
                    {
                        return Err(Error::ForkChecksum);
                    }
                    self.stage = Stage::Done;
                }
                Stage::Done => return Ok(Symbol::End)
            }
        }
    }
}


/// Fallible owned storage without unsafe allocation glue. One state stays at a
/// stable heap address; moving a streaming request must not copy its arithmetic
/// models and transform counters, or enlarge every non-Arsenic decoder.
pub(super) struct Arsenic
{
    state: Vec<State>
}
impl Arsenic
{
    pub fn new(length: u64) -> Result<Self>
    {
        let mut state = Vec::new();
        state.try_reserve_exact(1).map_err(|_| Error::Allocation)?;
        state.push(State::new(length));
        Ok(Self { state })
    }

    pub fn next(&mut self, input: &mut Input<'_>) -> Result<Symbol>
    {
        self.state.first_mut().ok_or(Error::InvalidState)?.next(input)
    }
}
