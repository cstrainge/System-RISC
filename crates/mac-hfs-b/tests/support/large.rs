//! Zero-eliding storage adapter: the filesystem still allocates every block and
//! receives every payload write. Used by core and public-SDK large-fork tests.
use std::collections::BTreeMap;

use mac_hfs_b::{time::Timestamp, *};
pub struct Sparse
{
    pub sectors: u64,
    pub data:    BTreeMap<u64, [u8; 512]>,
    pub written: u64
}
impl BlockDevice for Sparse
{
    fn sector_count(&self) -> u64
    {
        self.sectors
    }

    fn read_sector(&mut self, sector: u64, out: &mut [u8; 512]) -> mac_block::Result<()>
    {
        if sector >= self.sectors
        {
            return Err(mac_block::Error::Malformed("bounds"));
        }
        *out = self.data.get(&sector).copied().unwrap_or([0; 512]);
        Ok(())
    }
}
impl WritableBlockDevice for Sparse
{
    fn is_read_only(&self) -> bool
    {
        false
    }

    fn write_sector(&mut self, sector: u64, input: &[u8; 512]) -> mac_block::Result<()>
    {
        if sector >= self.sectors
        {
            return Err(mac_block::Error::Malformed("bounds"));
        }
        self.written += 512;
        if input.iter().all(|&b| b == 0)
        {
            self.data.remove(&sector);
        }
        else
        {
            self.data.insert(sector, *input);
        }
        Ok(())
    }

    fn flush(&mut self) -> mac_block::Result<()>
    {
        Ok(())
    }
}

pub fn allocated_fork() -> (Sparse, u32)
{
    let mut disk = Sparse { sectors: 6 * 1024 * 1024 * 1024 / 512,
                            data:    BTreeMap::new(),
                            written: 0 };
    let partition = Partition { start:   0,
                                sectors: disk.sectors };
    let budget = Budget::default();
    format(&mut disk,
           partition,
           FormatOptions { name:    "Large",
                           created: Timestamp(0),
                           uuid:    [9; 16] },
           budget).unwrap();
    let id;
    {
        let mut v = Volume::mount(&mut disk, partition, budget).unwrap();
        id = v.create(2, "large data", false, Timestamp(0)).unwrap().id;
        let zeroes = vec![0; 1024 * 1024];
        for chunk in 0..4096u64
        {
            v.write_fork(id, ForkKind::Data, chunk * 1024 * 1024, &zeroes, Timestamp(chunk))
             .unwrap();
        }
        v.write_fork(id, ForkKind::Data, 1u64 << 32, &[0; 4096], Timestamp(4096)).unwrap();
        for boundary in [1u64 << 31, 1u64 << 32]
        {
            v.write_fork(id, ForkKind::Data, boundary - 8, b"boundary-crossed", Timestamp(1))
             .unwrap();
        }
        assert_eq!(v.entry(id).unwrap().data.length, (1u64 << 32) + 4096);
        v.check().unwrap();
    }
    (disk, id)
}
