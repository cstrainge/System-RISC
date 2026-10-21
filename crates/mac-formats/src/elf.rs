//! Strict build-time ELF64 little-endian RV64IMAC extraction; never executes code.
pub mod relocation;
use alloc::vec::Vec;

use crate::{Error, Result, slice};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment
{
    pub offset:      u64,
    pub address:     u64,
    pub file_size:   u64,
    pub memory_size: u64,
    pub flags:       u32,
    pub alignment:   u64
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image
{
    pub entry:    u64,
    pub flags:    u32,
    pub segments: Vec<Segment>
}

fn u16le(bytes: &[u8], offset: usize) -> Result<u16>
{
    Ok(u16::from_le_bytes(slice(bytes, offset, 2)?.try_into().unwrap()))
}
fn u32le(bytes: &[u8], offset: usize) -> Result<u32>
{
    Ok(u32::from_le_bytes(slice(bytes, offset, 4)?.try_into().unwrap()))
}
fn u64le(bytes: &[u8], offset: usize) -> Result<u64>
{
    Ok(u64::from_le_bytes(slice(bytes, offset, 8)?.try_into().unwrap()))
}

pub fn decode(bytes: &[u8], load_base: u64, load_limit: u64) -> Result<Image>
{
    slice(bytes, 0, 64)?;
    // LLVM marks retained SDK declaration sections SHF_GNU_RETAIN and sets
    // OSABI GNU. Both System V and GNU use the same supported static load/reloc
    // contracts; this build-only marker is never copied into native resources.
    if &bytes[..7] != b"\x7fELF\x02\x01\x01" || !matches!(bytes[7], 0 | 3)
    {
        return Err(Error("expected ELF64 little-endian static System V/GNU image"));
    }
    if u16le(bytes, 16)? != 2 || u16le(bytes, 18)? != 243 || u32le(bytes, 20)? != 1
    {
        return Err(Error("expected fixed-address RISC-V executable"));
    }
    let flags = u32le(bytes, 48)?;
    if flags & !1 != 0
    {
        return Err(Error("unsupported RISC-V ABI flags (requires soft-float RV64IMAC)"));
    }
    if u16le(bytes, 52)? != 64 || u16le(bytes, 54)? != 56
    {
        return Err(Error("invalid ELF header sizes"));
    }
    let count = u16le(bytes, 56)? as usize;
    if count == 0 || count > 256 || load_base >= load_limit
    {
        return Err(Error("invalid ELF program count or memory range"));
    }
    let start =
        usize::try_from(u64le(bytes, 32)?).map_err(|_| Error("ELF program offset overflow"))?;
    let headers = slice(bytes, start, count * 56)?;
    let entry = u64le(bytes, 24)?;
    let mut segments: Vec<Segment> = Vec::new();
    for h in headers.as_chunks::<56>().0
    {
        let kind = u32le(h, 0)?;
        if kind == 2 || kind == 3 || kind == 7
        {
            return Err(Error("dynamic linking, interpreters and TLS are not supported"));
        }
        if kind != 1
        {
            continue;
        }
        let s = Segment { flags:       u32le(h, 4)?,
                          offset:      u64le(h, 8)?,
                          address:     u64le(h, 16)?,
                          file_size:   u64le(h, 32)?,
                          memory_size: u64le(h, 40)?,
                          alignment:   u64le(h, 48)? };
        if s.memory_size == 0
        {
            continue;
        }
        let end = s.address.checked_add(s.memory_size).ok_or(Error("ELF memory size overflow"))?;
        if u64le(h, 24)? != s.address
           || s.address < load_base
           || end > load_limit
           || s.file_size > s.memory_size
        {
            return Err(Error("ELF segment outside reserved memory or invalid sizes"));
        }
        if s.flags & !7 != 0 || s.flags & 3 == 3
        {
            return Err(Error("invalid or writable executable segment"));
        }
        if s.alignment > 1
           && (!s.alignment.is_power_of_two() || s.address % s.alignment != s.offset % s.alignment)
        {
            return Err(Error("invalid ELF segment alignment"));
        }
        slice(bytes,
              usize::try_from(s.offset).map_err(|_| Error("ELF file offset overflow"))?,
              usize::try_from(s.file_size).map_err(|_| Error("ELF file size overflow"))?)?;
        if segments
            .iter()
            .any(|other| s.address < other.address + other.memory_size && other.address < end)
        {
            return Err(Error("overlapping ELF load segments"));
        }
        segments.push(s);
    }
    if entry & 1 != 0
       || !segments.iter().any(|s| {
                              s.flags & 1 != 0
                              && entry >= s.address
                              && entry < s.address + s.file_size
                          })
    {
        return Err(Error("entry point is not in executable file data"));
    }
    segments.sort_by_key(|s| s.address);
    Ok(Image { entry,
               flags,
               segments })
}

#[cfg(test)]
mod tests
{
    use alloc::vec;

    use super::*;

    fn fixture() -> Vec<u8>
    {
        let mut b = vec![0; 124];
        b[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        b[16..18].copy_from_slice(&2u16.to_le_bytes());
        b[18..20].copy_from_slice(&243u16.to_le_bytes());
        b[20..24].copy_from_slice(&1u32.to_le_bytes());
        b[24..32].copy_from_slice(&0x80200000u64.to_le_bytes());
        b[32..40].copy_from_slice(&64u64.to_le_bytes());
        b[52..54].copy_from_slice(&64u16.to_le_bytes());
        b[54..56].copy_from_slice(&56u16.to_le_bytes());
        b[56..58].copy_from_slice(&1u16.to_le_bytes());
        b[64..68].copy_from_slice(&1u32.to_le_bytes());
        b[68..72].copy_from_slice(&5u32.to_le_bytes());
        b[72..80].copy_from_slice(&120u64.to_le_bytes());
        b[80..88].copy_from_slice(&0x80200000u64.to_le_bytes());
        b[88..96].copy_from_slice(&0x80200000u64.to_le_bytes());
        b[96..104].copy_from_slice(&4u64.to_le_bytes());
        b[104..112].copy_from_slice(&8u64.to_le_bytes());
        b
    }

    #[test]
    fn llvm_retained_contract_sections_allow_gnu_osabi_only_at_build_time()
    {
        let mut bytes = fixture();
        bytes[7] = 3;
        assert!(decode(&bytes, 0x80200000, 0x81000000).is_ok());
        bytes[7] = 9;
        assert!(decode(&bytes, 0x80200000, 0x81000000).is_err());
    }
    #[test]
    fn validates_machine_entry_ranges_and_bss()
    {
        let bytes = fixture();
        let image = decode(&bytes, 0x80200000, 0x81000000).unwrap();
        assert_eq!(image.segments[0].memory_size - image.segments[0].file_size, 4);
        for len in 0..bytes.len()
        {
            assert!(decode(&bytes[..len], 0x80200000, 0x81000000).is_err());
        }
        for (offset, value) in [(4, 1), (5, 2), (18, 62), (48, 4), (68, 7), (24, 1)]
        {
            let mut bad = bytes.clone();
            bad[offset] = value;
            assert!(decode(&bad, 0x80200000, 0x81000000).is_err(), "offset {offset}");
        }
        assert!(decode(&bytes, 0x81000000, 0x82000000).is_err());
    }
}
