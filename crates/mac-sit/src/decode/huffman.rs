//! StuffIt method 3: MSB-first preorder binary tree, followed by symbols.
//! Iterative construction rejects trees beyond 256 leaves / 511 nodes.
use alloc::vec::Vec;

use super::{Input, Symbol};
use crate::{Error, Result};

#[derive(Clone, Copy)]
struct Node
{
    children: [u16; 2],
    leaf:     Option<u8>
}
pub(super) struct Huffman
{
    nodes:   Vec<Node>,
    slots:   Vec<(u16, usize)>,
    leaf:    Option<(u16, u8, u8)>,
    current: u16,
    byte:    u8,
    bits:    u8,
    built:   bool
}
impl Huffman
{
    pub(super) fn new() -> Result<Self>
    {
        let mut nodes = Vec::new();
        nodes.try_reserve_exact(511).map_err(|_| Error::Allocation)?;
        let mut slots = Vec::new();
        slots.try_reserve_exact(256).map_err(|_| Error::Allocation)?;
        slots.push((u16::MAX, 0));
        Ok(Self { nodes,
                  slots,
                  leaf: None,
                  current: 0,
                  byte: 0,
                  bits: 0,
                  built: false })
    }

    fn bit(&mut self, input: &mut Input<'_>) -> Result<Option<u8>>
    {
        if self.bits == 0
        {
            let Some(byte) = input.byte()
            else
            {
                return if input.exhausted() { Err(Error::Truncated) } else { Ok(None) };
            };
            self.byte = byte;
            self.bits = 8;
        }
        self.bits -= 1;
        Ok(Some((self.byte >> self.bits) & 1))
    }

    pub(super) fn next(&mut self, input: &mut Input<'_>, finished: bool) -> Result<Symbol>
    {
        while !self.built
        {
            if let Some((node, mut value, mut count)) = self.leaf
            {
                while count < 8
                {
                    let Some(bit) = self.bit(input)?
                    else
                    {
                        self.leaf = Some((node, value, count));
                        return Ok(Symbol::Input);
                    };
                    value = (value << 1) | bit;
                    count += 1;
                }
                self.nodes[usize::from(node)].leaf = Some(value);
                self.leaf = None;
            }
            else if let Some(&(parent, branch)) = self.slots.last()
            {
                let Some(bit) = self.bit(input)?
                else
                {
                    return Ok(Symbol::Input);
                };
                if self.nodes.len() == 511
                {
                    return Err(Error::Limit);
                }
                self.slots.pop();
                let id = self.nodes.len() as u16;
                self.nodes.push(Node { children: [u16::MAX; 2],
                                       leaf:     None });
                if parent != u16::MAX
                {
                    self.nodes[usize::from(parent)].children[branch] = id;
                }
                if bit == 1
                {
                    self.leaf = Some((id, 0, 0));
                }
                else
                {
                    if self.slots.len() > 254
                    {
                        return Err(Error::Limit);
                    }
                    self.slots.push((id, 1));
                    self.slots.push((id, 0));
                }
            }
            else
            {
                self.built = true;
            }
        }
        if finished
        {
            // Only padding bits in the last byte are allowed after the output.
            return if input.exhausted() { Ok(Symbol::End) } else { Err(Error::Length) };
        }
        loop
        {
            let node = *self.nodes.get(usize::from(self.current)).ok_or(Error::CompressedData)?;
            if let Some(value) = node.leaf
            {
                self.current = 0;
                return Ok(Symbol::Byte(value));
            }
            let Some(bit) = self.bit(input)?
            else
            {
                return Ok(Symbol::Input);
            };
            self.current = node.children[usize::from(bit)];
        }
    }
}
