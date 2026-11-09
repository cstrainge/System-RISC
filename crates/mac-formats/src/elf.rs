//! Strict build-time ELF64 little-endian RV64 LP64 extraction; never executes code.
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
        return Err(Error::Malformed("expected ELF64 little-endian static System V/GNU image"));
    }
    // e_machine and e_flags are the architecture's own ELF identity, and this
    // decoder is shared by every loader. It accepts the machines this
    // repository produces and validates each one's flags against that machine's
    // contract, rather than assuming one architecture's values.
    let machine = u16le(bytes, 18)?;
    if u16le(bytes, 16)? != 2 || u32le(bytes, 20)? != 1
    {
        return Err(Error::Malformed("expected a fixed-address executable"));
    }
    let flags = u32le(bytes, 48)?;
    match machine
    {
        // RV64: only the RVC compressed-instruction flag is meaningful.
        243 =>
        {
            if flags & !1 != 0
            {
                return Err(Error::Malformed("unsupported RISC-V ABI flags (requires the LP64 \
                                             calling convention; rebuild with the System RISC \
                                             guest profile)"));
            }
        }
        // AArch64: e_flags carries no ABI variation this loader accepts.
        183 =>
        {
            if flags != 0
            {
                return Err(Error::Malformed("unsupported AArch64 ABI flags (rebuild with the \
                                             System RISC guest profile)"));
            }
        }
        _ => return Err(Error::Malformed("expected an executable for a supported machine"))
    }
    if u16le(bytes, 52)? != 64 || u16le(bytes, 54)? != 56
    {
        return Err(Error::Malformed("invalid ELF header sizes"));
    }
    let count = u16le(bytes, 56)? as usize;
    if count == 0 || count > 256 || load_base >= load_limit
    {
        return Err(Error::Malformed("invalid ELF program count or memory range"));
    }
    let start = usize::try_from(u64le(bytes, 32)?).map_err(|_| {
                                                      Error::Malformed("ELF program offset \
                                                                        overflow")
                                                  })?;
    let headers = slice(bytes, start, count * 56)?;
    let entry = u64le(bytes, 24)?;
    let mut segments: Vec<Segment> = Vec::new();
    for h in headers.as_chunks::<56>().0
    {
        let kind = u32le(h, 0)?;
        if kind == 2 || kind == 3 || kind == 7
        {
            return Err(Error::Malformed("dynamic linking, interpreters and TLS are not \
                                         supported"));
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
        let end = s.address
                   .checked_add(s.memory_size)
                   .ok_or(Error::Malformed("ELF memory size overflow"))?;
        if u64le(h, 24)? != s.address
           || s.address < load_base
           || end > load_limit
           || s.file_size > s.memory_size
        {
            return Err(Error::Malformed("ELF segment outside reserved memory or invalid sizes"));
        }
        if s.flags & !7 != 0 || s.flags & 3 == 3
        {
            return Err(Error::Malformed("invalid or writable executable segment"));
        }
        if s.alignment > 1
           && (!s.alignment.is_power_of_two() || s.address % s.alignment != s.offset % s.alignment)
        {
            return Err(Error::Malformed("invalid ELF segment alignment"));
        }
        slice(bytes,
              usize::try_from(s.offset).map_err(|_| {
                                           Error::Malformed("ELF file offset overflow")
                                       })?,
              usize::try_from(s.file_size).map_err(|_| {
                                              Error::Malformed("ELF file size overflow")
                                          })?)?;
        if segments
            .iter()
            .any(|other| s.address < other.address + other.memory_size && other.address < end)
        {
            return Err(Error::Malformed("overlapping ELF load segments"));
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
        return Err(Error::Malformed("entry point is not in executable file data"));
    }
    segments.sort_by_key(|s| s.address);
    Ok(Image { entry,
               flags,
               segments })
}

