//! StuffIt method 13: little-endian Huffman codes and a 64 KiB LZ window.
//! Parsing state, including partially read meta-codes, survives arbitrary I/O
//! boundaries. Tables are built once per fork; literals and matches share them.
use alloc::vec::Vec;

use super::{Input, Symbol};
use crate::{Error, Result};
#[path = "lz_huffman_tables.rs"]
mod tables;
use tables::*;
const MISSING: u16 = u16::MAX;
const WINDOW: usize = 65536;
const MAX_NODES: usize = 321 * 32 + 1;
#[derive(Clone, Copy)]
struct Node
{
    child:  [u16; 2],
    symbol: u16
}
impl Node
{
    const EMPTY: Self = Self { child:  [MISSING; 2],
                               symbol: MISSING };
}
struct Tree
{
    nodes:   Vec<Node>,
    current: u16
}
impl Tree
{
    fn new(symbols: usize) -> Result<Self>
    {
        let mut nodes = Vec::new();
        nodes.try_reserve_exact(2 * symbols + 32).map_err(|_| Error::Allocation)?;
        nodes.push(Node::EMPTY);
        Ok(Self { nodes,
                  current: 0 })
    }

    fn insert(&mut self, symbol: u16, code: u32, length: u8, little: bool) -> Result<()>
    {
        let mut node = 0usize;
        for bit in 0..length
        {
            if self.nodes[node].symbol != MISSING
            {
                return Err(Error::CompressedData);
            }
            let branch = ((code >> if little { bit } else { length - 1 - bit }) & 1) as usize;
            if self.nodes[node].child[branch] == MISSING
            {
                if self.nodes.len() == MAX_NODES
                {
                    return Err(Error::Limit);
                }
                self.nodes.try_reserve(1).map_err(|_| Error::Allocation)?;
                let next = self.nodes.len() as u16;
                self.nodes.push(Node::EMPTY);
                self.nodes[node].child[branch] = next;
            }
            node = self.nodes[node].child[branch] as usize;
        }
        if self.nodes[node].symbol != MISSING || self.nodes[node].child != [MISSING; 2]
        {
            return Err(Error::CompressedData);
        }
        self.nodes[node].symbol = symbol;
        Ok(())
    }

    fn canonical(lengths: &[i8]) -> Result<Self>
    {
        if lengths.len() > 321 || lengths.iter().any(|&length| !(-1..=32).contains(&length))
        {
            return Err(Error::CompressedData);
        }
        let mut tree = Self::new(lengths.len())?;
        let mut code = 0u64;
        for length in 1..=32
        {
            for (symbol, &bits) in lengths.iter().enumerate()
            {
                if bits != length
                {
                    continue;
                }
                if code >= 1u64 << length
                {
                    return Err(Error::CompressedData);
                }
                tree.insert(symbol as u16, code as u32, length as u8, false)?;
                code += 1;
            }
            code <<= 1;
        }
        Ok(tree)
    }

    fn meta() -> Result<Self>
    {
        let mut tree = Self::new(META_CODES.len())?;
        for (symbol, (&code, &length)) in
            META_CODES.iter().zip(META_CODE_LENGTHS.iter()).enumerate()
        {
            tree.insert(symbol as u16, code, length as u8, true)?;
        }
        Ok(tree)
    }

    fn next(&mut self, bits: &mut Bits, input: &mut Input<'_>) -> Result<Option<u16>>
    {
        loop
        {
            let node = self.nodes.get(self.current as usize).ok_or(Error::CompressedData)?;
            if node.symbol != MISSING
            {
                let symbol = node.symbol;
                self.current = 0;
                return Ok(Some(symbol));
            }
            let Some(bit) = bits.read(1, input)?
            else
            {
                return Ok(None);
            };
            self.current = node.child[bit as usize];
            if self.current == MISSING
            {
                return Err(Error::CompressedData);
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
            self.value |= u64::from(byte) << self.count;
            self.count += 8;
        }
        let value = (self.value & ((1u64 << count) - 1)) as u32;
        self.value >>= count;
        self.count -= count;
        Ok(Some(value))
    }
}
#[derive(Clone, Copy)]
enum Stage
{
    Header,
    Lengths,
    LengthExtra(u8),
    Repeat,
    Symbol,
    Length(u8),
    Offset,
    Distance(u8),
    Match,
    End
}
pub(super) struct LzHuffman
{
    stage:        Stage,
    bits:         Bits,
    trees:        [Option<Tree>; 3],
    meta:         Option<Tree>,
    shared:       bool,
    table:        usize,
    lengths:      Vec<i8>,
    wanted:       usize,
    offset_codes: usize,
    length:       i8,
    repeat:       usize,
    second:       bool,
    history:      Vec<u8>,
    position:     usize,
    match_length: usize,
    distance:     usize
}
impl LzHuffman
{
    pub fn new() -> Result<Self>
    {
        let mut history = Vec::new();
        history.try_reserve_exact(WINDOW).map_err(|_| Error::Allocation)?;
        history.resize(WINDOW, 0);
        let mut lengths = Vec::new();
        lengths.try_reserve_exact(321).map_err(|_| Error::Allocation)?;
        Ok(Self { stage: Stage::Header,
                  bits: Bits::default(),
                  trees: [None, None, None],
                  meta: None,
                  shared: false,
                  table: 0,
                  lengths,
                  wanted: 321,
                  offset_codes: 0,
                  length: 0,
                  repeat: 0,
                  second: false,
                  history,
                  position: 0,
                  match_length: 0,
                  distance: 0 })
    }

    fn finish_table(&mut self) -> Result<()>
    {
        self.trees[self.table] = Some(Tree::canonical(&self.lengths)?);
        self.lengths.clear();
        self.length = 0;
        self.table = if self.table == 0 && self.shared { 2 } else { self.table + 1 };
        if self.table == 3
        {
            self.meta = None;
            self.stage = Stage::Symbol;
        }
        else
        {
            self.wanted = if self.table == 2 { self.offset_codes } else { 321 };
            self.stage = Stage::Lengths;
        }
        Ok(())
    }

    fn emit(&mut self, byte: u8) -> Symbol
    {
        self.history[self.position] = byte;
        self.position = (self.position + 1) & (WINDOW - 1);
        Symbol::Byte(byte)
    }

    pub fn next(&mut self, input: &mut Input<'_>, finished: bool) -> Result<Symbol>
    {
        if finished
        {
            if matches!(self.stage, Stage::Match) && self.match_length != 0
            {
                return Err(Error::Length);
            }
            // Logical length is authoritative; the optional end code and byte
            // padding are not expanded as extra content. Validate CRC outside.
            while input.byte().is_some()
            {}
            return Ok(if input.exhausted() { Symbol::End } else { Symbol::Input });
        }
        loop
        {
            if !input.work()
            {
                return Ok(Symbol::Yield);
            }
            match self.stage
            {
                Stage::Header =>
                {
                    let Some(header) = self.bits.read(8, input)?
                    else
                    {
                        return Ok(Symbol::Input);
                    };
                    let book = (header >> 4) as usize;
                    if book == 0
                    {
                        self.shared = header & 8 != 0;
                        self.offset_codes = (header & 7) as usize + 10;
                        self.meta = Some(Tree::meta()?);
                        self.stage = Stage::Lengths;
                    }
                    else if book <= 5
                    {
                        self.trees[0] = Some(Tree::canonical(FIRST_CODE_LENGTHS[book - 1])?);
                        self.trees[1] = Some(Tree::canonical(SECOND_CODE_LENGTHS[book - 1])?);
                        self.trees[2] = Some(Tree::canonical(OFFSET_CODE_LENGTHS[book - 1])?);
                        self.stage = Stage::Symbol;
                    }
                    else
                    {
                        return Err(Error::CompressedData);
                    }
                }
                Stage::Lengths =>
                {
                    let Some(symbol) = self.meta
                                           .as_mut()
                                           .ok_or(Error::InvalidState)?
                                           .next(&mut self.bits, input)?
                    else
                    {
                        return Ok(Symbol::Input);
                    };
                    match symbol
                    {
                        0..=30 =>
                        {
                            self.length = symbol as i8 + 1;
                            self.repeat = 1;
                            self.stage = Stage::Repeat;
                        }
                        31 =>
                        {
                            self.length = -1;
                            self.repeat = 1;
                            self.stage = Stage::Repeat;
                        }
                        32 | 33 =>
                        {
                            self.length = if symbol == 32
                                          {
                                              self.length.checked_add(1)
                                          }
                                          else
                                          {
                                              self.length.checked_sub(1)
                                          }.filter(|length| (-1..=32).contains(length))
                                           .ok_or(Error::CompressedData)?;
                            self.repeat = 1;
                            self.stage = Stage::Repeat;
                        }
                        34..=36 => self.stage = Stage::LengthExtra(symbol as u8),
                        _ => return Err(Error::CompressedData)
                    }
                }
                Stage::LengthExtra(symbol) =>
                {
                    let count = match symbol
                    {
                        34 => 1,
                        35 => 3,
                        36 => 6,
                        _ => return Err(Error::InvalidState)
                    };
                    let Some(value) = self.bits.read(count, input)?
                    else
                    {
                        return Ok(Symbol::Input);
                    };
                    self.repeat = value as usize
                                  + match symbol
                                  {
                                      34 => 1,
                                      35 => 3,
                                      36 => 11,
                                      _ => return Err(Error::InvalidState)
                                  };
                    self.stage = Stage::Repeat;
                }
                Stage::Repeat =>
                {
                    if self.repeat > self.wanted - self.lengths.len()
                    {
                        return Err(Error::CompressedData);
                    }
                    if self.repeat != 0
                    {
                        self.lengths.push(self.length);
                        self.repeat -= 1;
                    }
                    else if self.lengths.len() == self.wanted
                    {
                        self.finish_table()?;
                    }
                    else
                    {
                        self.stage = Stage::Lengths;
                    }
                }
                Stage::Symbol =>
                {
                    let table = usize::from(self.second && !self.shared);
                    let Some(symbol) = self.trees[table].as_mut()
                                                        .ok_or(Error::InvalidState)?
                                                        .next(&mut self.bits, input)?
                    else
                    {
                        return Ok(Symbol::Input);
                    };
                    if symbol < 256
                    {
                        self.second = false;
                        return Ok(self.emit(symbol as u8));
                    }
                    self.second = true;
                    match symbol
                    {
                        256..=317 =>
                        {
                            self.match_length = (symbol - 253) as usize;
                            self.stage = Stage::Offset;
                        }
                        318 => self.stage = Stage::Length(10),
                        319 => self.stage = Stage::Length(15),
                        320 => self.stage = Stage::End,
                        _ => return Err(Error::CompressedData)
                    }
                }
                Stage::Length(count) =>
                {
                    let Some(length) = self.bits.read(count, input)?
                    else
                    {
                        return Ok(Symbol::Input);
                    };
                    self.match_length = length as usize + 65;
                    self.stage = Stage::Offset;
                }
                Stage::Offset =>
                {
                    let Some(count) = self.trees[2].as_mut()
                                                   .ok_or(Error::InvalidState)?
                                                   .next(&mut self.bits, input)?
                    else
                    {
                        return Ok(Symbol::Input);
                    };
                    if count > 16
                    {
                        return Err(Error::CompressedData);
                    }
                    if count == 0
                    {
                        self.distance = 1;
                        self.stage = Stage::Match;
                    }
                    else
                    {
                        self.stage = Stage::Distance(count as u8);
                    }
                }
                Stage::Distance(count) =>
                {
                    let Some(extra) = self.bits.read(count - 1, input)?
                    else
                    {
                        return Ok(Symbol::Input);
                    };
                    self.distance = (1usize << (count - 1)) + extra as usize + 1;
                    self.stage = Stage::Match;
                }
                Stage::Match =>
                {
                    if self.match_length == 0
                    {
                        self.stage = Stage::Symbol;
                        continue;
                    }
                    let byte =
                        self.history[(self.position + WINDOW - self.distance) & (WINDOW - 1)];
                    self.match_length -= 1;
                    return Ok(self.emit(byte));
                }
                Stage::End => return Ok(Symbol::End)
            }
        }
    }
}
