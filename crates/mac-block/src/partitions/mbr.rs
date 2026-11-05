//! Checked 512-byte master-boot-record ranges, independent of filesystems.
//!
//! A medium that carries an MBR states its extents in the first sector's four
//! entry slots. This is the reader for the writer the image tool implements,
//! kept beside the Apple map reader so both table formats are parsed once
//! rather than per consumer.
use alloc::vec::Vec;

use crate::*;

/// One MBR entry. `kind` is the partition type byte, which a caller compares
/// against the type its platform record declares.
pub struct MbrPartition
{
    pub range:    Partition,
    pub kind:     u8,
    pub bootable: bool
}

const SIGNATURE: [u8; 2] = [0x55, 0xAA];
const ENTRY_BYTES: usize = 16;
const FIRST_ENTRY: usize = 446;
const SECTORS: usize = 4;

fn word(b: &[u8], at: usize) -> u32
{
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// Every non-empty entry of the first sector's partition table.
///
/// A sector without the boot signature is refused rather than treated as an
/// empty table: a medium with no partition table at all is a different layout,
/// and a consumer that silently saw zero partitions would report a readable disk
/// as having nothing on it.
pub fn mbr_partitions(disk: &mut impl BlockDevice) -> Result<Vec<MbrPartition>>
{
    let mut sector = [0; 512];
    disk.read_sector(0, &mut sector)?;
    if sector[510..512] != SIGNATURE
    {
        return Err(Error::Malformed("no master boot record signature"));
    }
    let mut out: Vec<MbrPartition> = Vec::new();
    out.try_reserve_exact(SECTORS)
       .map_err(|_| Error::Malformed("partition allocation failed"))?;
    for index in 0..SECTORS
    {
        let at = FIRST_ENTRY + index * ENTRY_BYTES;
        let kind = sector[at + 4];
        let start = u64::from(word(&sector, at + 8));
        let sectors = u64::from(word(&sector, at + 12));
        // An unused slot is all zeroes; a slot with a type but no extent is
        // malformed rather than empty.
        if kind == 0 && start == 0 && sectors == 0
        {
            continue;
        }
        if kind == 0 || sectors == 0
        {
            return Err(Error::Malformed("incomplete master boot record entry"));
        }
        if start.saturating_add(sectors) > disk.sector_count()
        {
            return Err(Error::Malformed("master boot record entry extends past the medium"));
        }
        out.push(MbrPartition { range: Partition { start,
                                                   sectors },
                                kind,
                                bootable: sector[at] == 0x80 });
    }
    Ok(out)
}
