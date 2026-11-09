//! Checked 512-byte Apple partition-map ranges, independent of filesystems.
use alloc::vec::Vec;

use crate::*;
pub struct ApplePartition
{
    pub range: Partition,
    pub kind:  [u8; 32]
}
fn long(b: &[u8], at: usize) -> u32
{
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}
pub fn apple_partitions(disk: &mut impl BlockDevice) -> Result<Vec<ApplePartition>>
{
    let mut b = [0; 512];
    if disk.sector_count() < 3
    {
        return Err(Error::Malformed("disk is too small"));
    }
    disk.read_sector(1, &mut b)?;
    if &b[..2] != b"PM"
    {
        return Err(Error::Malformed("no Apple partition map"));
    }
    let count = u64::from(long(&b, 4));
    if count == 0 || count > 128 || count >= disk.sector_count()
    {
        return Err(Error::Malformed("invalid Apple partition count"));
    }
    let mut out: Vec<ApplePartition> = Vec::new();
    out.try_reserve_exact(count as usize)
       .map_err(|_| Error::Malformed("partition allocation failed"))?;
    for index in 1..=count
    {
        disk.read_sector(index, &mut b)?;
        if &b[..2] != b"PM" || u64::from(long(&b, 4)) != count
        {
            return Err(Error::Malformed("invalid Apple partition entry"));
        }
        let start = u64::from(long(&b, 8));
        let sectors = u64::from(long(&b, 12));
        // Partition-map records themselves are not volume candidates.
        let kind: [u8; 32] = b[48..80].try_into().unwrap();
        if kind.split(|&b| b == 0).next() == Some(b"Apple_partition_map".as_slice())
        {
            continue;
        }
        if sectors == 0
        {
            continue;
        }
        if start < count + 1
           || start.checked_add(sectors).is_none_or(|end| end > disk.sector_count())
        {
            return Err(Error::Malformed("partition outside device"));
        }
        let offset = u64::from(long(&b, 80));
        let length = u64::from(long(&b, 84));
        let length = if length == 0
        {
            sectors.checked_sub(offset).ok_or(Error::Malformed("invalid partition data offset"))?
        }
        else
        {
            length
        };
        if length == 0 || offset.checked_add(length).is_none_or(|end| end > sectors)
        {
            return Err(Error::Malformed("partition logical data outside partition"));
        }
        let range = Partition { start:   start + offset,
                                sectors: length };
        range.validate(disk)?;
        if out.iter().any(|p| {
                         range.start < p.range.start + p.range.sectors
                         && p.range.start < range.start + range.sectors
                     })
        {
            return Err(Error::Malformed("overlapping Apple partition data"));
        }
        out.push(ApplePartition { range,
                                  kind });
    }
    Ok(out)
}


