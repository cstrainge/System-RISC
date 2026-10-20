//! Shared RLOC validation, independent of build or runtime image containers.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use crate::{Error, Result, slice};
fn u32le(bytes: &[u8], at: usize) -> Result<u32>
{
    Ok(u32::from_le_bytes(slice(bytes, at, 4)?.try_into().unwrap()))
}
#[derive(Debug)]
pub struct Patch
{
    pub address: u64,
    pub value:   u64,
    pub width:   u8
}
/// Validate all patches before the loader writes a single byte of application RAM.
pub fn patches(table: &[u8], base: u64, extent: u64, destination: u64,
               mut read: impl FnMut(u64, u8) -> Result<u64>)
               -> Result<Vec<Patch>>
{
    if table.get(..4) != Some(b"RLC1")
    {
        return Err(Error("invalid relocation resource"));
    }
    let count = u32le(table, 4)? as usize;
    if count > 65536 || table.len() != 8 + count * 8
    {
        return Err(Error("invalid relocation count"));
    }
    let mut output = Vec::new();
    let mut previous = 0;
    for (i, record) in table[8..].as_chunks::<8>().0.iter().enumerate()
    {
        let offset = u32le(record, 0)? as u64;
        let width = record[4];
        if !matches!(width, 4 | 8) || record[5..] != [0, 0, 0] || i != 0 && offset < previous
        {
            return Err(Error("invalid or overlapping relocation"));
        }
        previous = offset + u64::from(width);
        let old = read(offset, width)?;
        if old < base
           || old > base.checked_add(extent).ok_or(Error("relocation extent overflow"))?
        {
            return Err(Error("relocation target outside image"));
        }
        let value =
            destination.checked_add(old - base).ok_or(Error("relocated pointer overflow"))?;
        if width == 4 && value > u64::from(u32::MAX)
        {
            return Err(Error("32-bit relocation overflow"));
        }
        output.push(Patch { address:
                                destination.checked_add(offset)
                                           .ok_or(Error("relocation destination overflow"))?,
                            value,
                            width });
    }
    Ok(output)
}
