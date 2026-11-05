//! Compact, checked relocation metadata for independent application images.
//!
//! The walk over the ELF -- sections, symbols, which relocations are internal,
//! the emitted table and its overlap validation -- is shared. What differs per
//! machine is which relocation types exist and what each one means, so that is
//! the seam: [`Set`], selected once by [`for_machine`].
//!
//! The dispatch is a factory, like `machine/mod.rs`. No architecture name
//! appears in the walk, so a third machine adds a module and one line to
//! `for_machine` rather than another arm in the middle of the logic.
#![forbid(unsafe_code)]
mod aarch64;
mod riscv64;

use alloc::{boxed::Box, collections::BTreeMap, vec::Vec};

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
pub(super) fn initialized_segment(image: &Image, address: u64, width: u64) -> Result<&Segment>
{
    let end = address.checked_add(width).ok_or(Error::Malformed("relocation address overflow"))?;
    image.segments
         .iter()
         .find(|s| address >= s.address && end <= s.address + s.file_size)
         .ok_or(Error::Malformed("relocation outside initialized image"))
}
pub(super) fn segment_offset(segment: &Segment, address: u64) -> Result<usize>
{
    usize::try_from(segment.offset + address - segment.address)
        .map_err(|_| Error::Malformed("relocation offset overflow"))
}
pub(super) fn file_offset(image: &Image, address: u64, width: u64) -> Result<usize>
{
    segment_offset(initialized_segment(image, address, width)?, address)
}
pub(super) fn instruction(bytes: &[u8], image: &Image, address: u64) -> Result<u32>
{
    let segment = initialized_segment(image, address, 4)?;
    if address & 1 != 0 || segment.flags & 1 == 0
    {
        return Err(Error::Malformed("GOT relocation instruction is not aligned initialized \
                                     executable code"));
    }
    u32le(bytes, segment_offset(segment, address)?)
}
/// One relocation record, already decoded to the facts a machine needs.
///
/// `internal` says the target is a section in this image rather than an
/// undefined or absolute symbol, which is the distinction every relocation
/// format turns on: an internal target moves with the image, an external one
/// does not.
pub struct Site<'a>
{
    pub kind:     u32,
    pub address:  u64,
    pub internal: bool,
    pub symbol:   &'a [u8],
    pub record:   &'a [u8]
}

/// The shared accumulators a machine writes into.
pub struct Records<'a>
{
    pub bytes:       &'a [u8],
    pub image:       &'a Image,
    pub base:        u64,
    pub role:        crate::native::Role,
    /// Absolute sites to patch, keyed by offset from the image base.
    pub entries:     &'a mut BTreeMap<u32, u8>,
    /// Paired PC-relative difference endpoints, keyed by address.
    pub differences: &'a mut BTreeMap<u64, u8>
}

/// One machine's relocation vocabulary.
///
/// `record` is called for every non-`NONE` record in a non-debug relocation
/// section. `finish` runs once after the walk, for machines whose GOT cells can
/// only be resolved once every site is known.
pub trait Set
{
    fn record(&mut self, site: &Site<'_>, out: &mut Records<'_>) -> Result<()>;

    fn finish(&mut self, _out: &mut Records<'_>) -> Result<()>
    {
        Ok(())
    }
}

/// The machine's relocation vocabulary, chosen from the ELF header.
///
/// The only place a machine number is matched. An unknown machine is refused
/// rather than decoded with another architecture's rules.
pub fn for_machine(machine: u16) -> Result<Box<dyn Set>>
{
    match machine
    {
        243 => Ok(Box::new(riscv64::Relocations::default())),
        183 => Ok(Box::new(aarch64::Relocations::default())),
        _ => Err(Error::Malformed("no relocation support for this machine"))
    }
}

/// Records an absolute patch site, refusing a duplicate the emitted table could
/// not express.
pub(super) fn absolute(out: &mut Records<'_>, address: u64, width: u8) -> Result<()>
{
    let offset =
        u32::try_from(address.checked_sub(out.base).ok_or(Error::Malformed("relocation before \
                                                                            image"))?)
            .map_err(|_| Error::Malformed("relocation too far"))?;
    if out.entries.insert(offset, width).is_some()
    {
        return Err(Error::Malformed("duplicate relocation"));
    }
    Ok(())
}

/// Records one endpoint of a paired PC-relative difference relocation.
pub(super) fn difference(out: &mut Records<'_>, address: u64, bit: u8) -> Result<()>
{
    let state = out.differences.entry(address).or_insert(0);
    if *state & bit != 0
    {
        return Err(Error::Malformed("PIC difference relocation repeats an endpoint"));
    }
    *state |= bit;
    Ok(())
}

/// Refuses an absolute patch site inside executable code, which the native PIC
/// contract forbids: every target dependency must be built with
/// `relocation-model=pic`.
pub(super) fn require_writable(out: &Records<'_>, address: u64) -> Result<()>
{
    if out.image
          .segments
          .iter()
          .any(|segment| {
                   segment.flags & 1 != 0
                   && address >= segment.address
                   && address < segment.address + segment.file_size
               })
    {
        return Err(Error::Malformed("native PIC contract forbids absolute relocation slots in \
                                     executable segments; rebuild every target dependency with \
                                     relocation-model=pic"));
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
    let mut set = for_machine(u16le(bytes, 18)?)?;
    let mut entries = BTreeMap::new();
    // PIC jump tables contain paired difference relocations. Both endpoints move
    // together, so their linked difference is invariant and needs no patch.
    let mut differences = BTreeMap::<u64, u8>::new();
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
            let site = Site { kind,
                              address,
                              internal,
                              symbol,
                              record };
            let mut out = Records { bytes,
                                    image,
                                    base,
                                    role,
                                    entries:     &mut entries,
                                    differences: &mut differences };
            set.record(&site, &mut out)?;
        }
    }
    {
        let mut out = Records { bytes,
                                image,
                                base,
                                role,
                                entries:     &mut entries,
                                differences: &mut differences };
        set.finish(&mut out)?;
    }
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

/// Checked named section metadata. The view borrows original ELF bytes; it
/// does not authorize loading. Consumers must match actual PT_LOAD placement.
#[derive(Clone, Copy, Debug)]
pub struct NamedSection<'a>
{
    pub data:        &'a [u8],
    pub flags:       u64,
    pub address:     u64,
    pub file_offset: u64,
    pub alignment:   u64
}

/// A named build-time section. Duplicate names and malformed string tables are
/// rejected; no section-table information is retained in native code resources.
pub fn named_section_view<'a>(bytes: &'a [u8], name: &[u8]) -> Result<Option<NamedSection<'a>>>
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
            let data = contents(bytes, header)?;
            let address = u64le(header, 16)?;
            address.checked_add(data.len() as u64)
                   .ok_or(Error::Malformed("ELF named section address overflow"))?;
            found = Some(NamedSection { data,
                                        flags: u64le(header, 8)?,
                                        address,
                                        file_offset: u64le(header, 24)?,
                                        alignment: u64le(header, 48)? });
        }
    }
    Ok(found)
}

/// Byte-only view retained for existing module metadata consumers.
pub fn named_section<'a>(bytes: &'a [u8], name: &[u8]) -> Result<Option<&'a [u8]>>
{
    Ok(named_section_view(bytes, name)?.map(|section| section.data))
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
