//! Minimal segment-stream inspection without allocation or operating-system services.
use crate::{Descriptor, Error, FORMAT_VERSION, Result, be16, be32};
pub const HEADER_SIZE: usize = 8;
pub const RECORD_SIZE: usize = 20;
pub const MAX_SEGMENTS: usize = 256;
pub const EXECUTE: u32 = 1;
pub const WRITE: u32 = 2;
pub const READ: u32 = 4;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segment
{
    pub offset:      u64,
    pub length:      usize,
    pub flags:       u32,
    pub data_offset: usize
}

/// Allocation-free inspection. Returns the exact end of the segment stream so
/// enclosing definition resources can frame their trailing RLOC table. Finish
/// validation before using emitted metadata to write any destination memory.
pub fn inspect(length: usize, descriptor: &Descriptor,
               mut read: impl FnMut(usize, &mut [u8]) -> Result<()>, mut emit: impl FnMut(Segment))
               -> Result<usize>
{
    descriptor.encode()?;
    let mut header = [0; HEADER_SIZE];
    read(0, &mut header)?;
    if &header[..4] == b"\x7fELF"
    {
        return Err(Error("ELF-backed code resources are unsupported; rebuild the system"));
    }
    if &header[..4] != b"RV64" || be16(&header, 4)? != FORMAT_VERSION
    {
        return Err(Error("invalid RV64 segment resource header"));
    }
    let count = usize::from(be16(&header, 6)?);
    if count == 0 || count > MAX_SEGMENTS
    {
        return Err(Error("invalid RV64 segment count"));
    }
    let reservation = descriptor.load_limit - descriptor.load_base;
    let mut at = HEADER_SIZE;
    let mut previous_end = 0;
    let mut entry_valid = false;
    for _ in 0..count
    {
        let mut record = [0; RECORD_SIZE];
        read(at, &mut record)?;
        at = at.checked_add(RECORD_SIZE).ok_or(Error("RV64 record overflow"))?;
        let offset = u64::from_be_bytes(record[..8].try_into().unwrap());
        let bytes = u64::from_be_bytes(record[8..16].try_into().unwrap());
        let flags = be32(&record, 16)?;
        let end = offset.checked_add(bytes).ok_or(Error("RV64 segment range overflow"))?;
        let size = usize::try_from(bytes).map_err(|_| Error("RV64 segment length overflow"))?;
        let data_end = at.checked_add(size).ok_or(Error("RV64 data offset overflow"))?;
        if bytes == 0 || offset < previous_end || end > reservation || data_end > length
        {
            return Err(Error("RV64 segments overlap, are truncated or exceed reservation"));
        }
        if flags & !7 != 0 || flags & (WRITE | EXECUTE) == (WRITE | EXECUTE)
        {
            return Err(Error("invalid or writable executable RV64 segment"));
        }
        let entry = descriptor.entry - descriptor.load_base;
        entry_valid |= flags & EXECUTE != 0 && entry >= offset && entry < end;
        emit(Segment { offset,
                       length: size,
                       flags,
                       data_offset: at });
        previous_end = end;
        at = data_end;
    }
    if !entry_valid
    {
        return Err(Error("native entry is not in executable segment data"));
    }
    Ok(at)
}
