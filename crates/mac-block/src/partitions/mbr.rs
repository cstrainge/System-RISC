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

const SIGNATURE: [u8; 2] = [0x55, 0xaa];
const ENTRY_BYTES: usize = 16;
const FIRST_ENTRY: usize = 446;
const SECTORS: usize = 4;

fn word(b: &[u8], at: usize) -> u32
{
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

/// One decoded MBR entry without extent validation. This is the allocation-free
/// form, used by a caller that only needs to report the table it found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MbrEntry
{
    pub index:    usize,
    pub kind:     u8,
    pub first:    u32,
    pub sectors:  u32,
    pub bootable: bool
}

/// Every non-empty entry of the first sector's partition table, without
/// checking the extents against a medium. The signature and the entry shape are
/// still checked, so a caller cannot mistake a sector with no table for one.
pub fn mbr_entries(sector: &[u8; crate::SECTOR_SIZE]) -> Result<[Option<MbrEntry>; SECTORS]>
{
    if sector[510..512] != SIGNATURE
    {
        return Err(Error::Malformed("no master boot record signature"));
    }
    let mut out = [None; SECTORS];
    for index in 0..SECTORS
    {
        let at = FIRST_ENTRY + index * ENTRY_BYTES;
        let kind = sector[at + 4];
        let first = word(sector, at + 8);
        let sectors = word(sector, at + 12);
        // An unused slot is all zeroes; a slot with a type but no extent is
        // malformed rather than empty.
        if kind == 0 && first == 0 && sectors == 0
        {
            continue;
        }
        if kind == 0 || sectors == 0
        {
            return Err(Error::Malformed("incomplete master boot record entry"));
        }
        out[index] = Some(MbrEntry { index,
                                     kind,
                                     first,
                                     sectors,
                                     bootable: sector[at] == 0x80 });
    }
    Ok(out)
}

/// Every non-empty entry of the first sector's partition table, checked against
/// the medium's capacity.
///
/// A sector without the boot signature is refused rather than treated as an
/// empty table: a medium with no partition table at all is a different layout,
/// and a consumer that silently saw zero partitions would report a readable disk
/// as having nothing on it.
pub fn mbr_partitions(disk: &mut impl BlockDevice) -> Result<Vec<MbrPartition>>
{
    let mut sector = [0; 512];
    disk.read_sector(0, &mut sector)?;
    let entries = mbr_entries(&sector)?;
    let mut out: Vec<MbrPartition> = Vec::new();
    out.try_reserve_exact(SECTORS).map_err(|_| Error::Malformed("partition allocation failed"))?;
    for entry in entries.iter().flatten()
    {
        if u64::from(entry.first).saturating_add(u64::from(entry.sectors)) > disk.sector_count()
        {
            return Err(Error::Malformed("master boot record entry extends past the medium"));
        }
        out.push(MbrPartition { range:    Partition { start:   u64::from(entry.first),
                                                      sectors: u64::from(entry.sectors) },
                                kind:     entry.kind,
                                bootable: entry.bootable });
    }
    Ok(out)
}
