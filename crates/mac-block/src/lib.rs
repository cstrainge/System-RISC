//! Architecture-neutral sector storage shared by filesystems and disk tools.
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;
use alloc::boxed::Box;
mod partitions;
pub use mac_formats::{Error, Result};
pub use partitions::{ApplePartition, MbrEntry, MbrPartition, apple_partitions, mbr_entries,
                     mbr_partitions};
pub const SECTOR_SIZE: usize = 512;

pub trait BlockDevice
{
    fn sector_count(&self) -> u64;
    fn read_sector(&mut self, sector: u64, output: &mut [u8; SECTOR_SIZE]) -> Result<()>;

    fn read_sectors(&mut self, sector: u64, output: &mut [[u8; SECTOR_SIZE]]) -> Result<()>
    {
        if sector.checked_add(output.len() as u64).is_none_or(|end| end > self.sector_count())
        {
            return Err(Error::Malformed("sector range outside device"));
        }
        for (index, chunk) in output.iter_mut().enumerate()
        {
            self.read_sector(sector + index as u64, chunk)?;
        }
        Ok(())
    }
}

/// Success from flush means all preceding writes reached durable storage.
pub trait WritableBlockDevice: BlockDevice
{
    fn is_read_only(&self) -> bool;
    fn write_sector(&mut self, sector: u64, input: &[u8; SECTOR_SIZE]) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
}

// A boxed device is a device. Bootloaders hold their storage behind a trait
// object so the disk backend can be chosen from the platform record without the
// loader naming a concrete driver, and the layers above it still take ordinary
// `impl BlockDevice` arguments.
impl<T: BlockDevice + ?Sized> BlockDevice for Box<T>
{
    fn sector_count(&self) -> u64
    {
        (**self).sector_count()
    }

    fn read_sector(&mut self, sector: u64, output: &mut [u8; SECTOR_SIZE]) -> Result<()>
    {
        (**self).read_sector(sector, output)
    }

    fn read_sectors(&mut self, sector: u64, output: &mut [[u8; SECTOR_SIZE]]) -> Result<()>
    {
        (**self).read_sectors(sector, output)
    }
}

impl<T: WritableBlockDevice + ?Sized> WritableBlockDevice for Box<T>
{
    fn is_read_only(&self) -> bool
    {
        (**self).is_read_only()
    }

    fn write_sector(&mut self, sector: u64, input: &[u8; SECTOR_SIZE]) -> Result<()>
    {
        (**self).write_sector(sector, input)
    }

    fn flush(&mut self) -> Result<()>
    {
        (**self).flush()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Partition
{
    pub start:   u64,
    pub sectors: u64
}
impl Partition
{
    pub fn validate(self, disk: &impl BlockDevice) -> Result<()>
    {
        if self.sectors == 0
           || self.start
                  .checked_add(self.sectors)
                  .is_none_or(|end| end > disk.sector_count() || end > u64::MAX / 512)
        {
            return Err(Error::Malformed("volume range outside device"));
        }
        Ok(())
    }
}
