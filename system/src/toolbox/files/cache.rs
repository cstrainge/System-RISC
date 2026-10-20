//! Small coherent sector cache owned by one mounted volume's serialized device.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

const CAPACITY: usize = 64;
struct Sector
{
    number: u64,
    used:   u64,
    bytes:  [u8; 512]
}
#[derive(Default)]
pub(super) struct Cache
{
    sectors: Vec<Sector>,
    clock:   u64
}
impl Cache
{
    pub fn get(&mut self, number: u64, output: &mut [u8; 512]) -> bool
    {
        let Some(sector) = self.sectors.iter_mut().find(|s| s.number == number)
        else
        {
            return false;
        };
        self.clock = self.clock.wrapping_add(1);
        sector.used = self.clock;
        *output = sector.bytes;
        true
    }

    pub fn put(&mut self, number: u64, bytes: &[u8; 512])
    {
        self.clock = self.clock.wrapping_add(1);
        let next = Sector { number,
                            used: self.clock,
                            bytes: *bytes };
        if let Some(sector) = self.sectors.iter_mut().find(|s| s.number == number)
        {
            *sector = next;
        }
        else if self.sectors.len() < CAPACITY
        {
            // A cache allocation failure leaves the successful I/O uncached.
            if self.sectors.try_reserve(1).is_ok()
            {
                self.sectors.push(next);
            }
        }
        else
        {
            let oldest = self.sectors
                             .iter()
                             .enumerate()
                             .max_by_key(|(_, s)| self.clock.wrapping_sub(s.used))
                             .unwrap()
                             .0;
            self.sectors[oldest] = next;
        }
    }

    pub fn clear(&mut self)
    {
        self.sectors.clear();
    }
}
