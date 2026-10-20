//! Architecture-neutral sector storage shared by filesystems and disk tools.
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;
mod partitions;
pub use mac_formats::{Error, Result};
pub use partitions::{ApplePartition, apple_partitions};
pub const SECTOR_SIZE: usize = 512;

pub trait BlockDevice
{
    fn sector_count(&self) -> u64;
    fn read_sector(&mut self, sector: u64, output: &mut [u8; SECTOR_SIZE]) -> Result<()>;
}

/// Success from flush means all preceding writes reached durable storage.
pub trait WritableBlockDevice: BlockDevice
{
    fn is_read_only(&self) -> bool;
    fn write_sector(&mut self, sector: u64, input: &[u8; SECTOR_SIZE]) -> Result<()>;
    fn flush(&mut self) -> Result<()>;
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
            return Err(Error("volume range outside device"));
        }
        Ok(())
    }
}
