//! Read-only classic HFS for firmware and future native disk utilities.
//! Sector I/O is supplied by the caller; this crate has no host dependencies.
#![no_std]
extern crate alloc;

mod volume;
use alloc::vec::Vec;

pub use mac_formats::{Error, Result};
pub use volume::{CatalogEntry, File, Folder, Fork, Volume, names_equal};

pub trait BlockDevice
{
    fn sector_count(&self) -> u64;
    fn read_sector(&mut self, sector: u64, output: &mut [u8; 512]) -> Result<()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Partition
{
    pub start:   u64,
    pub sectors: u64
}

pub(crate) fn bytes(data: &[u8], offset: usize, length: usize) -> Result<&[u8]>
{
    data.get(offset..offset.checked_add(length).ok_or(Error("HFS offset overflow"))?)
        .ok_or(Error("truncated HFS structure"))
}
pub(crate) fn be16(data: &[u8], offset: usize) -> Result<u16>
{
    Ok(u16::from_be_bytes(bytes(data, offset, 2)?.try_into().unwrap()))
}
pub(crate) fn be32(data: &[u8], offset: usize) -> Result<u32>
{
    Ok(u32::from_be_bytes(bytes(data, offset, 4)?.try_into().unwrap()))
}

/// An unpartitioned HFS volume, or Apple_HFS entries in a 512-byte APM.
pub fn partitions(disk: &mut impl BlockDevice) -> Result<Vec<Partition>>
{
    disk.sector_count().checked_mul(512).ok_or(Error("disk byte size overflow"))?;
    if disk.sector_count() < 3
    {
        return Err(Error("disk is too small"));
    }
    let mut sector = [0; 512];
    disk.read_sector(2, &mut sector)?;
    if &sector[..2] == b"BD"
    {
        return Ok(alloc::vec![Partition { start:   0,
                                          sectors: disk.sector_count() }]);
    }
    disk.read_sector(1, &mut sector)?;
    if &sector[..2] != b"PM"
    {
        return Err(Error("no classic HFS volume or Apple partition map"));
    }
    let count = be32(&sector, 4)? as u64;
    if count == 0 || count > 128 || count >= disk.sector_count()
    {
        return Err(Error("invalid Apple partition count"));
    }
    let mut result = Vec::new();
    for index in 1..=count
    {
        disk.read_sector(index, &mut sector)?;
        if &sector[..2] != b"PM" || be32(&sector, 4)? as u64 != count
        {
            return Err(Error("invalid Apple partition entry"));
        }
        let kind = &sector[48..80];
        if kind.split(|&b| b == 0).next() != Some(b"Apple_HFS".as_slice())
        {
            continue;
        }
        let start = be32(&sector, 8)? as u64;
        let sectors = be32(&sector, 12)? as u64;
        if sectors < 3
           || start < count + 1
           || start.checked_add(sectors).is_none_or(|end| end > disk.sector_count())
        {
            return Err(Error("HFS partition outside disk"));
        }
        let data_start = be32(&sector, 80)? as u64;
        let data_count = be32(&sector, 84)? as u64;
        let data_count = if data_count == 0
        {
            sectors.checked_sub(data_start).ok_or(Error("invalid Apple partition data offset"))?
        }
        else
        {
            data_count
        };
        if data_count < 3 || data_start.checked_add(data_count).is_none_or(|end| end > sectors)
        {
            return Err(Error("Apple partition logical data outside partition"));
        }
        result.push(Partition { start:   start + data_start,
                                sectors: data_count });
    }
    Ok(result)
}

pub(crate) fn read_at(disk: &mut impl BlockDevice, offset: u64, output: &mut [u8]) -> Result<()>
{
    let limit = disk.sector_count().checked_mul(512).ok_or(Error("disk size overflow"))?;
    if offset.checked_add(output.len() as u64).is_none_or(|end| end > limit)
    {
        return Err(Error("disk read outside device"));
    }
    let mut sector = [0; 512];
    let mut done = 0;
    while done < output.len()
    {
        let position = offset + done as u64;
        let within = position as usize & 511;
        let length = (512 - within).min(output.len() - done);
        disk.read_sector(position / 512, &mut sector)?;
        output[done..done + length].copy_from_slice(&sector[within..within + length]);
        done += length;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
