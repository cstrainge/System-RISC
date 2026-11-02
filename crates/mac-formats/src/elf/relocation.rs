//! Compact, checked RV64 relocation metadata for independent application images.
#![forbid(unsafe_code)]
use alloc::{collections::BTreeMap, vec::Vec};

use super::*;

fn section(bytes: &[u8], index: usize) -> Result<&[u8]>
{
    let count = usize::from(u16le(bytes, 60)?);
    if index >= count || u16le(bytes, 58)? != 64
    {
        return Err(Error::Malformed("invalid ELF section table"));
    }
    let start = usize::try_from(u64le(bytes, 40)?).map_err(|_| {
                                                      Error::Malformed("ELF section offset \
                                                                        overflow")
                                                  })?;
    slice(bytes, start.checked_add(index * 64).ok_or(Error::Malformed("ELF section overflow"))?, 64)
}
fn contents<'a>(bytes: &'a [u8], header: &[u8]) -> Result<&'a [u8]>
{
    slice(bytes,
          usize::try_from(u64le(header, 24)?).map_err(|_| {
                                                 Error::Malformed("section offset overflow")
                                             })?,
          usize::try_from(u64le(header, 32)?).map_err(|_| {
                                                 Error::Malformed("section size overflow")
                                             })?)
}
fn symbol<'a>(bytes: &'a [u8], table: &[u8], index: usize) -> Result<&'a [u8]>
{
    if u32le(table, 4)? != 2 || u64le(table, 56)? != 24
    {
        return Err(Error::Malformed("invalid relocation symbols"));
    }
    slice(contents(bytes, table)?,
          index.checked_mul(24).ok_or(Error::Malformed("symbol offset overflow"))?,
          24)
}
fn initialized_segment(image: &Image, address: u64, width: u64) -> Result<&Segment>
{
    let end = address.checked_add(width).ok_or(Error::Malformed("relocation address overflow"))?;
    image.segments
         .iter()
         .find(|s| address >= s.address && end <= s.address + s.file_size)
         .ok_or(Error::Malformed("relocation outside initialized image"))
}
fn segment_offset(segment: &Segment, address: u64) -> Result<usize>
{
    usize::try_from(segment.offset + address - segment.address)
        .map_err(|_| Error::Malformed("relocation offset overflow"))
}
fn file_offset(image: &Image, address: u64, width: u64) -> Result<usize>
{
    segment_offset(initialized_segment(image, address, width)?, address)
}
fn instruction(bytes: &[u8], image: &Image, address: u64) -> Result<u32>
{
    let segment = initialized_segment(image, address, 4)?;
    if address & 1 != 0 || segment.flags & 1 == 0
    {
        return Err(Error::Malformed("GOT relocation instruction is not aligned initialized \
                                     executable code"));
    }
    u32le(bytes, segment_offset(segment, address)?)
}
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
fn got_cells(bytes: &[u8], image: &Image, base: u64, highs: &mut [GotHigh], mut lows: Vec<GotLow>,
             pc_highs: &mut [u64], entries: &mut BTreeMap<u32, u8>)
             -> Result<()>
{
    use core::ops::Bound::{Excluded, Unbounded};
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
    let image_end =
        base.checked_add(span(image, base)?).ok_or(Error::Malformed("GOT image span overflows"))?;
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
        if low.kind != 24
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
            return Err(Error::Malformed("GOT relocation does not identify a matching AUIPC/LD \
                                         register pair"));
        }
        let upper_offset = i64::from((upper & 0xfffff000) as i32);
        let lower_offset = i64::from((load as i32) >> 20);
        let cell = high.address
                       .checked_add_signed(upper_offset)
                       .and_then(|address| address.checked_add_signed(lower_offset))
                       .ok_or(Error::Malformed("resolved GOT cell address overflows"))?;
        let segment = initialized_segment(image, cell, 8)?;
        if cell & 7 != 0 || segment.flags & 1 != 0
        {
            return Err(Error::Malformed("resolved GOT cell is not aligned initialized \
                                         nonexecutable data"));
        }
        let pointer = u64le(bytes, segment_offset(segment, cell)?)?;
        if high.target < base || high.target > image_end || pointer != high.target
        {
            return Err(Error::Malformed("resolved GOT cell does not contain its exact internal \
                                         symbol target"));
        }
        let offset = u32::try_from(cell.checked_sub(base).ok_or(Error::Malformed("GOT cell precedes image"))?)
            .map_err(|_| Error::Malformed("GOT cell offset exceeds native relocation range"))?;
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
                return Err(Error::Malformed("GOT cell overlaps a non-64-bit absolute relocation"));
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
pub fn span(image: &Image, base: u64) -> Result<u64>
{
    let end = image.segments
                   .iter()
                   .map(|s| s.address + s.memory_size)
                   .max()
                   .ok_or(Error::Malformed("empty image"))?;
    end.checked_sub(base)
       .and_then(|n| n.checked_add(4095))
       .map(|n| n & !4095)
       .ok_or(Error::Malformed("image span overflow"))
}
/// Build tools call this before stripping symbols. Old fixed images without the
/// linker marker deliberately receive no relocation capability.
pub fn extract(bytes: &[u8], image: &Image, base: u64, role: crate::native::Role)
               -> Result<Option<Vec<u8>>>
{
    if u64le(bytes, 40)? == 0 || u16le(bytes, 60)? == 0
    {
        return Ok(None);
    }
    let count = usize::from(u16le(bytes, 60)?);
    let mut marked = false;
    for i in 0..count
    {
        let table = section(bytes, i)?;
        if u32le(table, 4)? != 2
        {
            continue;
        }
        let strings = contents(bytes, section(bytes, u32le(table, 40)? as usize)?)?;
        let symbols = contents(bytes, table)?;
        if u64le(table, 56)? != 24 || !symbols.len().is_multiple_of(24)
        {
            return Err(Error::Malformed("invalid symbol table"));
        }
        for symbol in symbols.as_chunks::<24>().0
        {
            let name = strings.get(u32le(symbol, 0)? as usize..)
                              .ok_or(Error::Malformed("invalid symbol name"))?;
            if name.starts_with(b"__system_risc_relocation_v1\0")
               && u16le(symbol, 6)? == 0xfff1
               && u64le(symbol, 8)? == 1
            {
                marked = true;
            }
        }
    }
    if !marked
    {
        return Ok(None);
    }
    let mut entries = BTreeMap::new();
    // PIC jump tables contain ADD32/SUB32 pairs. Both internal endpoints move
    // together, so their linked difference is invariant and needs no patch.
    let mut differences = BTreeMap::<u64, u8>::new();
    let mut got_high = Vec::new();
    let mut got_low = Vec::new();
    let mut pc_high = Vec::new();
    for i in 0..count
    {
        let header = section(bytes, i)?;
        if u32le(header, 4)? != 4
        {
            continue;
        }
        let target = section(bytes, u32le(header, 44)? as usize)?;
        if u64le(target, 8)? & 2 == 0
        {
            continue;
        } // Debug relocations do not execute.
        if u64le(header, 56)? != 24
        {
            return Err(Error::Malformed("invalid relocation entry size"));
        }
        let records = contents(bytes, header)?;
        if !records.len().is_multiple_of(24)
        {
            return Err(Error::Malformed("truncated relocation table"));
        }
        let table = section(bytes, u32le(header, 40)? as usize)?;
        for record in records.as_chunks::<24>().0
        {
            let address = u64le(record, 0)?;
            let info = u64le(record, 8)?;
            let kind = info as u32;
            if kind == 0
            {
                continue;
            }
            let symbol = symbol(bytes, table, (info >> 32) as usize)?;
            let shndx = u16le(symbol, 6)?;
            let internal = shndx != 0
                           && shndx != 0xfff1
                           && usize::from(shndx) < count
                           && u64le(section(bytes, usize::from(shndx))?, 8)? & 2 != 0;
            match kind
            {
                1 | 2 =>
                {
                    if shndx == 0xfff1
                    {
                        continue;
                    } // A literal absolute symbol stays literal.
                    if !internal
                    {
                        return Err(Error::Malformed("external absolute relocation"));
                    }
                    if kind == 1 && role == crate::native::Role::CodeResource
                    {
                        return Err(Error::Malformed("RV64 module/definition pointers require \
                                                     64-bit storage to support System-selected \
                                                     virtual addresses; rebuild without \
                                                     internal 32-bit absolute pointers"));
                    }
                    let width = if kind == 1 { 4 } else { 8 };
                    file_offset(image, address, width)?;
                    if image.segments.iter().any(|segment| {
                                                segment.flags & 1 != 0
                                                && address >= segment.address
                                                && address < segment.address + segment.file_size
                                            })
                    {
                        return Err(Error::Malformed("native PIC contract forbids absolute \
                                                     relocation slots in executable segments; \
                                                     rebuild every target dependency with \
                                                     relocation-model=pic"));
                    }
                    let offset=u32::try_from(address.checked_sub(base).ok_or(Error::Malformed("relocation before image"))?).map_err(|_|Error::Malformed("relocation too far"))?;
                    if entries.insert(offset, width as u8).is_some()
                    {
                        return Err(Error::Malformed("duplicate relocation"));
                    }
                }
                35 | 39 =>
                {
                    if !internal
                    {
                        return Err(Error::Malformed("PIC difference relocation has an external \
                                                     endpoint"));
                    }
                    file_offset(image, address, 4)?;
                    let bit = if kind == 35 { 1 } else { 2 };
                    let state = differences.entry(address).or_insert(0);
                    if *state & bit != 0
                    {
                        return Err(Error::Malformed("PIC difference relocation repeats an \
                                                     endpoint"));
                    }
                    *state |= bit;
                }
                // PC-relative calls/address materialization and branches are
                // unchanged when both ends move together. Relaxation is disabled.
                20 =>
                {
                    if !internal
                    {
                        return Err(Error::Malformed("GOT_HI20 relocation has an external or \
                                                     absolute symbol"));
                    }
                    file_offset(image, address, 4)?;
                    got_high.try_reserve(1).map_err(|_| {
                                                Error::AllocationFailed("GOT high relocation \
                                                                         metadata allocation \
                                                                         failed")
                                            })?;
                    got_high.push(GotHigh { address,
                                            target: relocated_symbol(symbol, record)?,
                                            paired: false });
                }
                23 =>
                {
                    if !internal
                    {
                        return Err(Error::Malformed("external PC-relative relocation"));
                    }
                    file_offset(image, address, 2)?;
                    pc_high.try_reserve(1).map_err(|_| {
                                               Error::AllocationFailed("PC-relative high \
                                                                        relocation metadata \
                                                                        allocation failed")
                                           })?;
                    pc_high.push(address);
                }
                24 | 25 =>
                {
                    if !internal
                    {
                        return Err(Error::Malformed("external PC-relative relocation"));
                    }
                    file_offset(image, address, 2)?;
                    got_low.try_reserve(1).map_err(|_| {
                                               Error::AllocationFailed("GOT low relocation \
                                                                        metadata allocation failed")
                                           })?;
                    got_low.push(GotLow { address,
                                          high: relocated_symbol(symbol, record)?,
                                          kind });
                }
                16 | 17 | 18 | 19 | 44 | 45 | 57 =>
                {
                    if !internal
                    {
                        return Err(Error::Malformed("external PC-relative relocation"));
                    }
                    file_offset(image, address, 2)?;
                }
                _ => return Err(Error::Malformed("unsupported application relocation"))
            }
        }
    }
    got_cells(bytes, image, base, &mut got_high, got_low, &mut pc_high, &mut entries)?;
    for (address, endpoints) in differences
    {
        if endpoints != 3
        {
            return Err(Error::Malformed("PIC difference relocation is missing its paired \
                                         endpoint"));
        }
        let offset =
            address.checked_sub(base).ok_or(Error::Malformed("PIC difference before image"))?;
        // Difference words and rebased pointer words may never overlap.
        if let Some((&at, &width)) = entries.range(..=u32::try_from(offset + 3).map_err(|_| {
                                                          Error::Malformed("PIC difference offset \
                                                                            overflow")
                                                      })?)
                                            .next_back()
           && u64::from(at) + u64::from(width) > offset
        {
            return Err(Error::Malformed("PIC difference overlaps an absolute relocation"));
        }
    }
    let mut result = b"RLC1".to_vec();
    result.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (offset, width) in entries
    {
        result.extend_from_slice(&offset.to_le_bytes());
        result.extend_from_slice(&[width, 0, 0, 0]);
    }
    // Validate the stripped image's linked values too, before publishing metadata.
    patches(bytes, image, &result, base, base)?;
    Ok(Some(result))
}
pub use crate::relocation::Patch;
pub fn patches(bytes: &[u8], image: &Image, table: &[u8], base: u64, destination: u64)
               -> Result<Vec<Patch>>
{
    crate::relocation::patches(table, base, span(image, base)?, destination, |offset, width| {
        let address =
            base.checked_add(offset).ok_or(Error::Malformed("relocation address overflow"))?;
        let source = file_offset(image, address, u64::from(width))?;
        if width == 4 { Ok(u64::from(u32le(bytes, source)?)) } else { u64le(bytes, source) }
    })
}

/// A named build-time section. Duplicate names and malformed string tables are
/// rejected; no section-table information is retained in native code resources.
pub fn named_section<'a>(bytes: &'a [u8], name: &[u8]) -> Result<Option<&'a [u8]>>
{
    let count = usize::from(u16le(bytes, 60)?);
    let names = contents(bytes, section(bytes, usize::from(u16le(bytes, 62)?))?)?;
    let mut found = None;
    for index in 0..count
    {
        let header = section(bytes, index)?;
        let offset = u32le(header, 0)? as usize;
        let tail = names.get(offset..).ok_or(Error::Malformed("invalid section name"))?;
        let end = tail.iter()
                      .position(|&b| b == 0)
                      .ok_or(Error::Malformed("unterminated section name"))?;
        if &tail[..end] == name
        {
            if found.is_some()
            {
                return Err(Error::Malformed("duplicate ELF section name"));
            }
            if u32le(header, 4)? != 1
            {
                return Err(Error::Malformed("expected section file data"));
            }
            found = Some(contents(bytes, header)?);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests
{
    use alloc::vec;

    use super::*;
    #[test]
    fn relocation_plan_checks_all_addresses_before_loading()
    {
        let image = Image { entry:    0x1000,
                            flags:    0,
                            segments: vec![Segment { offset:      0,
                                                     address:     0x1000,
                                                     file_size:   16,
                                                     memory_size: 32,
                                                     flags:       6,
                                                     alignment:   1 }] };
        let bytes = [0x1008u64.to_le_bytes(), 0x1010u64.to_le_bytes()].concat();
        let table = [b"RLC1".as_slice(),
                     &2u32.to_le_bytes(),
                     &[0, 0, 0, 0, 8, 0, 0, 0, 8, 0, 0, 0, 8, 0, 0, 0]].concat();
        let patches = patches(&bytes, &image, &table, 0x1000, 0x5000).unwrap();
        assert_eq!(patches.iter().map(|p| (p.address, p.value, p.width)).collect::<Vec<_>>(),
                   vec![(0x5000, 0x5008, 8), (0x5008, 0x5010, 8)]);
        for length in 0..table.len()
        {
            assert!(super::patches(&bytes, &image, &table[..length], 0x1000, 0x5000).is_err());
        }
        for (at, value) in [(0, b'X'), (12, 3), (13, 1), (16, 4), (16, 16)]
        {
            let mut bad = table.clone();
            bad[at] = value;
            assert!(super::patches(&bytes, &image, &bad, 0x1000, 0x5000).is_err());
        }
        let mut invalid = bytes.clone();
        invalid[8..16].copy_from_slice(&0x9999u64.to_le_bytes());
        assert!(super::patches(&invalid, &image, &table, 0x1000, 0x5000).is_err());
        let mut narrow = table[..16].to_vec();
        narrow[4..8].copy_from_slice(&1u32.to_le_bytes());
        narrow[12] = 4;
        assert!(super::patches(&bytes, &image, &narrow, 0x1000, 0xffff_fff8).is_err());
    }
}
