//! The RISC-V binding of the shared inspection walk.
//!
//! The walk itself lives in `mac_kernel_model::pages::walk` because it is the
//! same work on any format; this module binds it to this architecture's
//! descriptor and layout so callers keep naming `Walker` and `Error<E>`.
pub use mac_kernel_model::pages::walk::{ReadTables, Span, Walker};

use crate::Error as EncodingError;

/// This architecture's encoding error is the one the walk reports.
pub type Error<E> = mac_kernel_model::pages::walk::Error<E, EncodingError>;

#[allow(unused_imports)]
use crate::{Decoded, Entry, LeafSize, Sv39, mapping::{Mapper, Tables}};
#[allow(unused_imports)]
use mac_kernel_model::pages::{PageOwnership, Permissions, PhysicalAddress};

#[cfg(test)]
mod tests
{
    extern crate std;
    use std::{boxed::Box, cell::Cell, collections::BTreeMap, string::ToString};

    use mac_kernel_model::pages::Access;

    use super::*;
    use crate::mapping::Mapper;
    struct Memory
    {
        frames: BTreeMap<u64, Box<[u64; 512]>>,
        reads:  Cell<usize>,
        fail:   Cell<Option<usize>>
    }
    impl Memory
    {
        fn new() -> Self
        {
            Self { frames: BTreeMap::new(),
                   reads:  Cell::new(0),
                   fail:   Cell::new(None) }
        }
    }
    impl Tables for Memory
    {
        type Error = &'static str;
        type Descriptor = crate::Entry;
        type Layout = crate::Sv39;

        fn allocate_zeroed(&mut self) -> Result<PhysicalAddress, Self::Error>
        {
            let address = (self.frames.len() as u64 + 1) * 4096;
            self.frames.insert(address, Box::new([0; 512]));
            Ok(PhysicalAddress(address))
        }

        fn load(&self, table: PhysicalAddress, index: usize) -> Result<u64, Self::Error>
        {
            self.reads.set(self.reads.get() + 1);
            if self.fail.get() == Some(self.reads.get())
            {
                self.fail.set(None);
                return Err("injected owned-table read refusal; retry is safe");
            }
            self.frames
                .get(&table.0)
                .and_then(|words| words.get(index))
                .copied()
                .ok_or("table is not owned by this root")
        }

        fn store(&mut self, table: PhysicalAddress, index: usize, entry: Entry)
                 -> Result<(), Self::Error>
        {
            *self.frames
                 .get_mut(&table.0)
                 .and_then(|w| w.get_mut(index))
                 .ok_or("foreign table or invalid index")? = entry.bits();
            Ok(())
        }
    }
    fn p() -> Permissions
    {
        Permissions { read:    true,
                      write:   true,
                      execute: false,
                      access:  Access::Supervisor,
                      global:  false }
    }
    #[test]
    fn coalesces_actual_leaves_skips_holes_and_snapshots_each_table_once()
    {
        let mut map = Mapper::new(Memory::new()).unwrap();
        map.map_range(0x400000, PhysicalAddress(0x800000), 8192, p(), PageOwnership::Manual)
           .unwrap();
        map.map_range(0x404000, PhysicalAddress(0x900000), 4096, p(), PageOwnership::Manual)
           .unwrap();
        let high = u64::MAX << 38;
        map.map_range(high, PhysicalAddress(0x40000000), 1 << 30, p(), PageOwnership::Manual)
           .unwrap();
        let (root, source) = map.into_tables();
        source.reads.set(0);
        let mut walk = Walker::new(&source, root, 0).unwrap();
        let first = walk.next_span().unwrap().unwrap();
        assert_eq!((first.virtual_base, first.physical_base.0, first.bytes),
                   (0x400000, 0x800000, 8192));
        let second = walk.next_span().unwrap().unwrap();
        assert_eq!((second.virtual_base, second.bytes), (0x404000, 4096));
        assert_eq!(walk.next_span().unwrap().unwrap().virtual_base, high);
        assert_eq!(walk.next_span().unwrap(), None);
        assert_eq!(walk.next_span().unwrap(), None);
        assert_eq!(source.reads.get(),
                   source.frames.len() * 512,
                   "no base-page RAM walk or table reread");
    }
    #[test]
    fn clipped_large_leaf_and_highest_page_finish_without_wrapping()
    {
        let mut map = Mapper::new(Memory::new()).unwrap();
        map.map_range(0, PhysicalAddress(0), 1 << 30, p(), PageOwnership::Manual).unwrap();
        map.map(u64::MAX - 4095,
                PhysicalAddress(4096),
                LeafSize::from_level(0).unwrap().bytes(),
                p(),
                PageOwnership::Manual)
           .unwrap();
        let (root, source) = map.into_tables();
        let mut walk = Walker::new(&source, root, 4096).unwrap();
        let span = walk.next_span().unwrap().unwrap();
        assert_eq!((span.virtual_base, span.physical_base.0, span.bytes),
                   (4096, 4096, (1 << 30) - 4096));
        let last = walk.next_span().unwrap().unwrap();
        assert_eq!((last.virtual_base, last.bytes), (u64::MAX - 4095, 4096));
        assert!(walk.next_span().unwrap().is_none());
    }
    #[test]
    fn partial_table_read_failure_retries_without_losing_pending_span()
    {
        let mut map = Mapper::new(Memory::new()).unwrap();
        map.map_range(0, PhysicalAddress(0), 4096, p(), PageOwnership::Manual).unwrap();
        map.map_range(1 << 30, PhysicalAddress(8192), 4096, p(), PageOwnership::Manual).unwrap();
        let (root, source) = map.into_tables();
        source.reads.set(0);
        source.fail.set(Some(3 * 512 + 17)); // after the first span, while finding its successor
        let mut walk = Walker::new(&source, root, 0).unwrap();
        let failure = walk.next_span().unwrap_err();
        assert!(failure.to_string().contains("owned-table read refusal"));
        assert_eq!(walk.next_span().unwrap().unwrap().virtual_base, 0);
        assert_eq!(walk.next_span().unwrap().unwrap().virtual_base, 1 << 30);
        assert!(walk.next_span().unwrap().is_none());
    }
    #[test]
    fn rejects_foreign_root_bad_encoding_and_noncanonical_start_with_detail()
    {
        let mut map = Mapper::new(Memory::new()).unwrap();
        map.map_range(0, PhysicalAddress(0), 4096, p(), PageOwnership::Manual).unwrap();
        let (root, mut source) = map.into_tables();
        assert!(Walker::new(&source, root, 1 << 39).err()
                                                   .unwrap()
                                                   .to_string()
                                                   .contains("canonical"));
        let mut foreign = Walker::new(&source, PhysicalAddress(0x100000), 0).unwrap();
        assert!(foreign.next_span().unwrap_err().to_string().contains("not owned"));
        source.frames.get_mut(&root.0).unwrap()[0] = 1 | 4; // W without R
        let mut corrupt = Walker::new(&source, root, 0).unwrap();
        assert!(matches!(corrupt.next_span(), Err(Error::Encoding(_))));
    }
}
