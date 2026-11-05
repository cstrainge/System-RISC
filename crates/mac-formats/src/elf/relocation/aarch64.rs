//! AArch64 relocation vocabulary.
//!
//! AArch64 PIC code addresses internal targets with an `ADRP` page and a
//! low-12-bit add or load, and external targets through a GOT cell. The pairs
//! are invariant when the image moves by whole pages, which is what the
//! platform record's page-aligned load bases provide; only absolute slots and
//! GOT cells need a patch.

use alloc::vec::Vec;

use super::{absolute, file_offset, initialized_segment, instruction, require_writable, segment_offset,
            Records, Set, Site};
use crate::{elf::*, Error, Result};

/// `R_AARCH64_ABS64`
const ABS64: u32 = 257;
/// `R_AARCH64_ABS32`
const ABS32: u32 = 258;
/// `R_AARCH64_PREL64` / `R_AARCH64_PREL32`
const PREL64: u32 = 260;
const PREL32: u32 = 261;
/// `R_AARCH64_ADR_PREL_LO21`
const ADR_PREL_LO21: u32 = 274;
/// `R_AARCH64_ADR_PREL_PG_HI21` and its no-check variant
const ADR_PREL_PG_HI21: u32 = 275;
const ADR_PREL_PG_HI21_NC: u32 = 276;
/// `R_AARCH64_ADD_ABS_LO12_NC`
const ADD_ABS_LO12_NC: u32 = 277;
/// The `LDST*_ABS_LO12_NC` family. The numbers are anchored on the encodings
/// actually observed in a built image: 285 addresses with `ldr w`, 286 with
/// `ldr x` and 299 with `ldr q`, so they are the 32-, 64- and 128-bit members.
const LDST8_ABS_LO12_NC: u32 = 278;
const LDST16_ABS_LO12_NC: u32 = 284;
const LDST32_ABS_LO12_NC: u32 = 285;
const LDST64_ABS_LO12_NC: u32 = 286;
const LDST128_ABS_LO12_NC: u32 = 299;
/// The PC-relative branch family. Observed with `b` at 282 and `bl` at 283;
/// both are PC-relative and move with the image, so the pairing is validated
/// from the instruction rather than trusted from the number.
const BRANCH_ONE: u32 = 282;
const BRANCH_TWO: u32 = 283;
/// `R_AARCH64_ADR_GOT_PAGE` / `R_AARCH64_LD64_GOT_LO12_NC`
const ADR_GOT_PAGE: u32 = 311;
const LD64_GOT_LO12_NC: u32 = 312;

/// The page size the ADRP/LO12 pairs are invariant under.
const PAGE: u64 = 4096;

struct GotPage
{
    address: u64,
    target:  u64,
    paired:  bool
}
struct GotLoad
{
    address: u64,
    high:    u64
}

fn relocated_symbol(symbol: &[u8], record: &[u8]) -> Result<u64>
{
    u64le(symbol, 8)?.checked_add_signed(u64le(record, 16)? as i64)
                     .ok_or(Error::Malformed("GOT relocation symbol/addend overflows"))
}

/// The page an `ADRP` addresses, given the instruction and its own address.
fn adrp_page(insn: u32, at: u64) -> Result<u64>
{
    // `1 immlo[2] 10000 immhi[19] Rd[5]`
    if insn & 0x9f00_0000 != 0x9000_0000
    {
        return Err(Error::Malformed("GOT page relocation is not an ADRP instruction"));
    }
    let immhi = i64::from((insn >> 5) & 0x7ffff);
    let immlo = i64::from((insn >> 29) & 0x3);
    // 21 signed bits, then scaled by the page size.
    let mut immediate = (immhi << 2) | immlo;
    if immediate & (1 << 20) != 0
    {
        immediate -= 1 << 21;
    }
    let base = at & !(PAGE - 1);
    base.checked_add_signed(immediate << 12)
        .ok_or(Error::Malformed("GOT page address overflows"))
}

/// The byte offset an `LDR Xt, [Xn, #imm]` adds to its base register.
fn ldr_offset(insn: u32) -> Result<(u64, u32, u32)>
{
    // `1111100101 imm12[12] Rn[5] Rt[5]` — 64-bit, unsigned offset, scaled by 8.
    if insn & 0xffc0_0000 != 0xf940_0000
    {
        return Err(Error::Malformed("GOT low relocation is not a 64-bit LDR immediate"));
    }
    let immediate = u64::from((insn >> 10) & 0xfff);
    Ok((immediate * 8, (insn >> 5) & 31, insn & 31))
}

#[derive(Default)]
pub(super) struct Relocations
{
    got_page: Vec<GotPage>,
    got_load: Vec<GotLoad>
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
            ABS64 | ABS32 =>
            {
                if shndx == 0xfff1
                {
                    return Ok(()); // A literal absolute symbol stays literal.
                }
                if !internal
                {
                    return Err(Error::Malformed("external absolute relocation"));
                }
                // A 32-bit absolute pointer cannot hold a System-selected
                // virtual address, so a code resource may not carry one.
                if kind == ABS32 && out.role == crate::native::Role::CodeResource
                {
                    return Err(Error::Malformed("AArch64 module/definition pointers require \
                                                 64-bit storage to support System-selected \
                                                 virtual addresses; rebuild without internal \
                                                 32-bit absolute pointers"));
                }
                let width = if kind == ABS32 { 4 } else { 8 };
                file_offset(out.image, address, width)?;
                require_writable(out, address)?;
                absolute(out, address, width as u8)?;
            }
            // S + A - P and the low 12 bits of S + A are unchanged when the
            // image moves by whole pages, so these need no patch. They are
            // still checked to address real initialized bytes of this image.
            PREL64 | PREL32 =>
            {
                if !internal
                {
                    return Err(Error::Malformed("external PC-relative relocation"));
                }
                file_offset(out.image, address, if kind == PREL64 { 8 } else { 4 })?;
            }
            ADR_PREL_LO21 | ADR_PREL_PG_HI21 | ADR_PREL_PG_HI21_NC | ADD_ABS_LO12_NC
            | LDST8_ABS_LO12_NC | LDST16_ABS_LO12_NC | LDST32_ABS_LO12_NC
            | LDST64_ABS_LO12_NC | LDST128_ABS_LO12_NC =>
            {
                if !internal
                {
                    return Err(Error::Malformed("external PC-relative relocation"));
                }
                // The instruction itself must be present and executable.
                instruction(out.bytes, out.image, address)?;
            }
            BRANCH_ONE | BRANCH_TWO =>
            {
                if !internal
                {
                    return Err(Error::Malformed("external branch relocation"));
                }
                // A PC-relative branch is invariant only if it really is one:
                // `b` is opcode 000101 and `bl` is 100101 in bits 31:26. The
                // relocation numbers are validated against the encoding rather
                // than trusted, so a wrong table cannot corrupt the image.
                let insn = instruction(out.bytes, out.image, address)?;
                let opcode = insn & 0xfc00_0000;
                if opcode != 0x1400_0000 && opcode != 0x9400_0000
                {
                    return Err(Error::Malformed("branch relocation does not address a `b` or \
                                                 `bl` instruction"));
                }
            }
            ADR_GOT_PAGE =>
            {
                if !internal
                {
                    return Err(Error::Malformed("GOT page relocation has an external or absolute \
                                                 symbol"));
                }
                instruction(out.bytes, out.image, address)?;
                self.got_page
                    .try_reserve(1)
                    .map_err(|_| {
                        Error::AllocationFailed("GOT page relocation metadata allocation failed")
                    })?;
                self.got_page
                    .push(GotPage { address,
                                    target: relocated_symbol(symbol, record)?,
                                    paired: false });
            }
            LD64_GOT_LO12_NC =>
            {
                if !internal
                {
                    return Err(Error::Malformed("external GOT load relocation"));
                }
                instruction(out.bytes, out.image, address)?;
                self.got_load
                    .try_reserve(1)
                    .map_err(|_| {
                        Error::AllocationFailed("GOT load relocation metadata allocation failed")
                    })?;
                self.got_load
                    .push(GotLoad { address,
                                    high: relocated_symbol(symbol, record)? });
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
    /// Resolves every GOT cell named by an `ADR_GOT_PAGE`/`LD64_GOT_LO12_NC`
    /// pair and records it as an eight-byte patch site.
    ///
    /// The pair is the only record that names the cell, so the cell's address
    /// has to be recovered from the instructions that address it: the `ADRP`
    /// page plus the `LDR` offset, with both naming the same register. The cell
    /// must hold exactly the symbol the relocation named.
    fn resolve_got(&mut self, out: &mut Records<'_>) -> Result<()>
    {
        // The ADRP/LO12 pairs above are invariant only for a page-aligned
        // image, so the base has to be page aligned before that reasoning holds.
        if out.base & (PAGE - 1) != 0
        {
            return Err(Error::Malformed("AArch64 image base is not page aligned; the page-relative \
                                         relocation pairs this decoder leaves unpatched are not \
                                         invariant"));
        }
        let (bytes, image, base) = (out.bytes, out.image, out.base);
        let pages = &mut self.got_page;
        let loads = core::mem::take(&mut self.got_load);

        pages.sort_unstable_by_key(|page| page.address);
        if pages.windows(2).any(|pair| pair[0].address == pair[1].address)
        {
            return Err(Error::Malformed("duplicate ADR_GOT_PAGE relocation site"));
        }
        let image_end = base.checked_add(super::span(image, base)?)
                            .ok_or(Error::Malformed("GOT image span overflows"))?;
        for load in loads
        {
            let Ok(at) = pages.binary_search_by_key(&load.high, |page| page.target)
            else
            {
                return Err(Error::Malformed("LD64_GOT_LO12_NC relocation has no matching \
                                             ADR_GOT_PAGE relocation"));
            };
            let page = &mut pages[at];
            let upper = instruction(bytes, image, page.address)?;
            let lower = instruction(bytes, image, load.address)?;
            let target_page = adrp_page(upper, page.address)?;
            let (offset, base_register, _) = ldr_offset(lower)?;
            if base_register == 31 || base_register != (upper & 31) || load.address == page.address
            {
                return Err(Error::Malformed("GOT relocation does not identify a matching \
                                             ADRP/LDR register pair"));
            }
            let cell = target_page.checked_add(offset)
                                  .ok_or(Error::Malformed("resolved GOT cell address \
                                                           overflows"))?;
            let segment = initialized_segment(image, cell, 8)?;
            if cell & 7 != 0 || segment.flags & 1 != 0
            {
                return Err(Error::Malformed("resolved GOT cell is not aligned initialized \
                                             nonexecutable data"));
            }
            let pointer = u64le(bytes, segment_offset(segment, cell)?)?;
            if page.target < base || page.target > image_end || pointer != page.target
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
            match out.entries.get(&offset)
            {
                Some(8) => (), // An absolute64 record or another GOT use of this cell.
                Some(_) =>
                {
                    return Err(Error::Malformed("GOT cell overlaps a non-64-bit absolute \
                                                 relocation"));
                }
                None =>
                {
                    if let Some((&other, &width)) = out.entries.range(..offset).next_back()
                       && u64::from(other) + u64::from(width) > u64::from(offset)
                    {
                        return Err(Error::Malformed("GOT cell overlaps another absolute \
                                                     relocation"));
                    }
                    out.entries.insert(offset, 8);
                }
            }
            page.paired = true;
        }
        if pages.iter().any(|page| !page.paired)
        {
            return Err(Error::Malformed("ADR_GOT_PAGE relocation has no matching \
                                         LD64_GOT_LO12_NC load"));
        }
        Ok(())
    }
}
