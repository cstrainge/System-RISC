//! RV64 relocation vocabulary.
//!
//! PIC relocation pairs are PC-relative and both ends move with the image, so
//! they need no patch; only absolute slots and GOT cells do. The GOT cell is
//! found by decoding the `AUIPC`/`LD` pair that addresses it, because the
//! `GOT_HI20` relocation is the only record that marks the cell.

use alloc::vec::Vec;

use super::{absolute, difference, file_offset, instruction, require_writable, Records, Set, Site,
            span};
use crate::{elf::*, Error, Result};

/// `R_RISCV_32`
const ABS32: u32 = 1;
/// `R_RISCV_64`
const ABS64: u32 = 2;
/// `R_RISCV_GOT_HI20`
const GOT_HI20: u32 = 20;
/// `R_RISCV_PCREL_HI20`
const PCREL_HI20: u32 = 23;
/// `R_RISCV_PCREL_LO12_I`
const PCREL_LO12_I: u32 = 24;
/// `R_RISCV_PCREL_LO12_S`
const PCREL_LO12_S: u32 = 25;
/// `R_RISCV_ADD32` / `R_RISCV_SUB32`
const ADD32: u32 = 35;
const SUB32: u32 = 39;

struct GotHigh
{
    address: u64,
    target:  u64,
    paired:  bool
}
struct GotLow
{
    address: u64,
    high:    u64,
    kind:    u32
}

fn relocated_symbol(symbol: &[u8], record: &[u8]) -> Result<u64>
{
    u64le(symbol, 8)?.checked_add_signed(u64le(record, 16)? as i64)
                     .ok_or(Error::Malformed("GOT relocation symbol/addend overflows"))
}

#[derive(Default)]
pub(super) struct Relocations
{
    got_high: Vec<GotHigh>,
    got_low:  Vec<GotLow>,
    pc_high:  Vec<u64>
}

impl Set for Relocations
{
    fn record(&mut self, site: &Site<'_>, out: &mut Records<'_>) -> Result<()>
    {
        let Site { kind,
                   address,
                   internal,
                   symbol,
                   record } = *site;
        let shndx = u16le(symbol, 6)?;
        match kind
        {
            ABS32 | ABS64 =>
            {
                if shndx == 0xfff1
                {
                    return Ok(()); // A literal absolute symbol stays literal.
                }
                if !internal
                {
                    return Err(Error::Malformed("external absolute relocation"));
                }
                if kind == ABS32 && out.role == crate::native::Role::CodeResource
                {
                    return Err(Error::Malformed("RV64 module/definition pointers require \
                                                 64-bit storage to support System-selected \
                                                 virtual addresses; rebuild without internal \
                                                 32-bit absolute pointers"));
                }
                let width = if kind == ABS32 { 4 } else { 8 };
                file_offset(out.image, address, width)?;
                require_writable(out, address)?;
                absolute(out, address, width as u8)?;
            }
            ADD32 | SUB32 =>
            {
                if !internal
                {
                    return Err(Error::Malformed("PIC difference relocation has an external \
                                                 endpoint"));
                }
                file_offset(out.image, address, 4)?;
                difference(out, address, if kind == ADD32 { 1 } else { 2 })?;
            }
            // PC-relative calls/address materialization and branches are
            // unchanged when both ends move together. Relaxation is disabled.
            GOT_HI20 =>
            {
                if !internal
                {
                    return Err(Error::Malformed("GOT_HI20 relocation has an external or absolute \
                                                 symbol"));
                }
                file_offset(out.image, address, 4)?;
                self.got_high
                    .try_reserve(1)
                    .map_err(|_| {
                        Error::AllocationFailed("GOT high relocation metadata allocation failed")
                    })?;
                self.got_high
                    .push(GotHigh { address,
                                    target: relocated_symbol(symbol, record)?,
                                    paired: false });
            }
            PCREL_HI20 =>
            {
                if !internal
                {
                    return Err(Error::Malformed("external PC-relative relocation"));
                }
                file_offset(out.image, address, 2)?;
                self.pc_high
                    .try_reserve(1)
                    .map_err(|_| {
                        Error::AllocationFailed("PC-relative high relocation metadata allocation \
                                                 failed")
                    })?;
                self.pc_high.push(address);
            }
            PCREL_LO12_I | PCREL_LO12_S =>
            {
                if !internal
                {
                    return Err(Error::Malformed("external PC-relative relocation"));
                }
                file_offset(out.image, address, 2)?;
                self.got_low
                    .try_reserve(1)
                    .map_err(|_| {
                        Error::AllocationFailed("GOT low relocation metadata allocation failed")
                    })?;
                self.got_low
                    .push(GotLow { address,
                                   high: relocated_symbol(symbol, record)?,
                                   kind });
            }
            // Fixed-width relocations against an internal target whose value is
            // invariant under a whole-image move.
            16 | 17 | 18 | 19 | 44 | 45 | 57 =>
            {
                if !internal
                {
                    return Err(Error::Malformed("external PC-relative relocation"));
                }
                file_offset(out.image, address, 2)?;
            }
            _ => return Err(Error::Malformed("unsupported application relocation type"))
        }
        Ok(())
    }

    fn finish(&mut self, out: &mut Records<'_>) -> Result<()>
    {
        self.resolve_got(out)
    }
}

impl Relocations
{
    /// Resolves every GOT cell named by a `GOT_HI20`/`PCREL_LO12_I` pair and
    /// records it as an eight-byte patch site.
    ///
    /// A `GOT_HI20` is the only relocation that names the cell, so the cell's
    /// address has to be recovered from the instructions that address it. The
    /// pair must agree: the same register, an `AUIPC` upper and an `LD` lower,
    /// and the cell must hold exactly the symbol the relocation named.
    fn resolve_got(&mut self, out: &mut Records<'_>) -> Result<()>
    {
        use core::ops::Bound::{Excluded, Unbounded};

        let (bytes, image, base) = (out.bytes, out.image, out.base);
        let highs = &mut self.got_high;
        let mut lows = core::mem::take(&mut self.got_low);
        let pc_highs = &mut self.pc_high;
        let entries = &mut *out.entries;

        highs.sort_unstable_by_key(|high| high.address);
        if highs.windows(2).any(|pair| pair[0].address == pair[1].address)
        {
            return Err(Error::Malformed("duplicate GOT_HI20 relocation site"));
        }
        pc_highs.sort_unstable();
        if pc_highs.windows(2).any(|pair| pair[0] == pair[1])
           || highs.iter().any(|high| pc_highs.binary_search(&high.address).is_ok())
        {
            return Err(Error::Malformed("duplicate PC-relative high relocation site"));
        }
        lows.sort_unstable_by_key(|low| low.address);
        if lows.windows(2).any(|pair| {
                              pair[0].address == pair[1].address
                              && pair.iter().any(|low| {
                                                highs.binary_search_by_key(&low.high, |high| {
                                                         high.address
                                                     })
                                                     .is_ok()
                                            })
                          })
        {
            return Err(Error::Malformed("duplicate GOT low relocation site"));
        }
        let image_end = base.checked_add(span(image, base)?)
                            .ok_or(Error::Malformed("GOT image span overflows"))?;
        for low in lows
        {
            let Ok(at) = highs.binary_search_by_key(&low.high, |high| high.address)
            else
            {
                if pc_highs.binary_search(&low.high).is_err()
                {
                    return Err(Error::Malformed("PCREL_LO12 relocation has no matching high \
                                                 relocation"));
                }
                continue; // An ordinary matched PCREL_HI20/LO12 is not a GOT reference.
            };
            let high = &mut highs[at];
            if low.kind != PCREL_LO12_I
            {
                return Err(Error::Malformed("GOT_HI20 requires a PCREL_LO12_I load, not an S-form \
                                             relocation"));
            }
            let upper = instruction(bytes, image, high.address)?;
            let load = instruction(bytes, image, low.address)?;
            let register = (upper >> 7) & 31;
            if upper & 0x7f != 0x17
               || register == 0
               || load & 0x707f != 0x3003
               || (load >> 15) & 31 != register
               || low.address == high.address
            {
                return Err(Error::Malformed("GOT relocation does not identify a matching \
                                             AUIPC/LD register pair"));
            }
            let upper_offset = i64::from((upper & 0xfffff000) as i32);
            let lower_offset = i64::from((load as i32) >> 20);
            let cell = high.address
                           .checked_add_signed(upper_offset)
                           .and_then(|address| address.checked_add_signed(lower_offset))
                           .ok_or(Error::Malformed("resolved GOT cell address overflows"))?;
            let segment = super::initialized_segment(image, cell, 8)?;
            if cell & 7 != 0 || segment.flags & 1 != 0
            {
                return Err(Error::Malformed("resolved GOT cell is not aligned initialized \
                                             nonexecutable data"));
            }
            let pointer = u64le(bytes, super::segment_offset(segment, cell)?)?;
            if high.target < base || high.target > image_end || pointer != high.target
            {
                return Err(Error::Malformed("resolved GOT cell does not contain its exact \
                                             internal symbol target"));
            }
            let offset = u32::try_from(cell.checked_sub(base)
                                           .ok_or(Error::Malformed("GOT cell precedes image"))?)
                             .map_err(|_| {
                                 Error::Malformed("GOT cell offset exceeds native relocation \
                                                   range")
                             })?;
            if let Some((&other, &width)) = entries.range(..offset).next_back()
               && u64::from(other) + u64::from(width) > u64::from(offset)
            {
                return Err(Error::Malformed("GOT cell overlaps another absolute relocation"));
            }
            if let Some((&other, _)) = entries.range((Excluded(offset), Unbounded)).next()
               && u64::from(other) < u64::from(offset) + 8
            {
                return Err(Error::Malformed("GOT cell overlaps another absolute relocation"));
            }
            match entries.get(&offset)
            {
                Some(8) => (), // Existing absolute64 record or another GOT use of this cell.
                Some(_) =>
                {
                    return Err(Error::Malformed("GOT cell overlaps a non-64-bit absolute \
                                                 relocation"));
                }
                None =>
                {
                    entries.insert(offset, 8);
                }
            }
            high.paired = true;
        }
        if highs.iter().any(|high| !high.paired)
        {
            return Err(Error::Malformed("GOT_HI20 relocation has no matching PCREL_LO12_I load"));
        }
        Ok(())
    }
}
