//! The RISC-V binding of the shared mapping walk.
//!
//! The walk itself lives in `mac_kernel_model::pages::mapping` because it is the
//! same work on any format; this module binds it to this architecture's
//! descriptor and layout so callers keep naming `Mapper<Frames>` and
//! `MapError<FrameError>`.
pub use mac_kernel_model::pages::mapping::{Mapper, Tables};

// The tests reach these through `use super::*`, mirroring what this module
// needed when the walk lived here.
#[allow(unused_imports)]
use core::fmt;
#[allow(unused_imports)]
use mac_kernel_model::{pages::{MappingPlan, PageLayout, PageOwnership, Permissions,
                               PhysicalAddress, VirtualAddress},
                       policy::{AddressRange, ModelError}};
#[allow(unused_imports)]
use crate::{Decoded, Entry, Error, LeafSize, Sv39};

/// This architecture's encoding error is the one the walk reports.
pub type MapError<E> = mac_kernel_model::pages::mapping::MapError<E, Error>;

#[cfg(test)]
mod tests
{
    extern crate alloc;
    use alloc::{boxed::Box, collections::BTreeMap, string::ToString};
    use core::cell::Cell;

    use mac_kernel_model::pages::{Access, MappingPlan, PageGeometry, VirtualAddress};

    use super::*;
    #[derive(Debug)]
    enum Refusal
    {
        Exhausted
        {
            used: usize, limit: usize
        },
        Foreign
        {
            address: u64
        },
        Slot
        {
            index: usize
        }
    }
    impl fmt::Display for Refusal
    {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
        {
            match self
            {
                Self::Exhausted { used,
                                  limit } =>
                {
                    write!(f, "table-frame budget exhausted: used {used}, limit {limit}")
                }
                Self::Foreign { address } =>
                {
                    write!(f, "table {address:#x} is not owned by this address space")
                }
                Self::Slot { index } => write!(f, "table slot {index} is outside 0..512")
            }
        }
    }
    struct Memory
    {
        frames: BTreeMap<u64, Box<[u64; 512]>>,
        limit:  usize,
        loads:  Cell<usize>,
        writes: usize
    }
    impl Memory
    {
        fn new(limit: usize) -> Self
        {
            Self { frames: BTreeMap::new(),
                   limit,
                   loads: Cell::new(0),
                   writes: 0 }
        }
    }
    impl Tables for Memory
    {
        type Error = Refusal;
        type Descriptor = Entry;
        type Layout = Sv39;

        fn allocate_zeroed(&mut self) -> Result<PhysicalAddress, Refusal>
        {
            if self.frames.len() == self.limit
            {
                return Err(Refusal::Exhausted { used:  self.frames.len(),
                                                limit: self.limit });
            }
            let address = (self.frames.len() as u64 + 1) * 4096;
            self.frames.insert(address, Box::new([0; 512]));
            Ok(PhysicalAddress(address))
        }

        fn load(&self, table: PhysicalAddress, index: usize) -> Result<u64, Refusal>
        {
            self.loads.set(self.loads.get() + 1);
            let frame = self.frames.get(&table.0).ok_or(Refusal::Foreign { address: table.0 })?;
            frame.get(index).copied().ok_or(Refusal::Slot { index })
        }

        fn store(&mut self, table: PhysicalAddress, index: usize, entry: Entry)
                 -> Result<(), Refusal>
        {
            let frame =
                self.frames.get_mut(&table.0).ok_or(Refusal::Foreign { address: table.0 })?;
            *frame.get_mut(index).ok_or(Refusal::Slot { index })? = entry.bits();
            self.writes += 1;
            Ok(())
        }
    }
    fn permissions() -> Permissions
    {
        Permissions { read:    true,
                      write:   true,
                      execute: false,
                      access:  Access::Supervisor,
                      global:  true }
    }
    #[test]
    fn extent_mapping_uses_large_leaves_and_preflights_invalid_spans()
    {
        let mut mapper = Mapper::new(Memory::new(5)).unwrap();
        let base = 0xffffffe000000000;
        assert_eq!(mapper.map_range(base + 0x3ffff000,
                                    PhysicalAddress(0x3ffff000),
                                    (1 << 30) + 8192,
                                    permissions(),
                                    PageOwnership::Manual)
                         .unwrap(),
                   (3, 3),
                   "boundary pages should surround one huge leaf");
        assert_eq!(mapper.tables().frames.len(), 5);
        let writes = mapper.tables().writes;
        assert_eq!(mapper.map_range(base + 0x3ffff000,
                                    PhysicalAddress(0x3ffff000),
                                    (1 << 30) + 8192,
                                    permissions(),
                                    PageOwnership::Manual)
                         .unwrap(),
                   (3, 0));
        assert_eq!(mapper.tables().writes, writes);
        let loads = mapper.tables().loads.get();
        assert!(matches!(mapper.map_range(0x400000,
                                          PhysicalAddress(0),
                                          0xffffffc000001000 - 0x400000,
                                          permissions(),
                                          PageOwnership::Manual),
                         Err(MapError::NonCanonicalSpan { .. })));
        let error =
            mapper.map_range(base, PhysicalAddress(0), 4097, permissions(), PageOwnership::Manual)
                  .unwrap_err();
        assert!(error.to_string().contains("VA 0xffffffe000000000 to PA 0x0 for 0x1001 bytes"));
        assert_eq!(mapper.tables().loads.get(), loads, "invalid range touched a table");
        assert_eq!(mapper.tables().writes, writes);
        assert_eq!(mapper.map_range(base,
                                    PhysicalAddress(0),
                                    0,
                                    permissions(),
                                    PageOwnership::Manual)
                         .unwrap(),
                   (0, 0));
        assert_eq!(mapper.tables().loads.get(), loads, "empty range did unnecessary work");
    }
    #[test]
    fn eight_gib_physmap_uses_eight_leaves_and_no_per_base_page_construction()
    {
        let memory = Memory::new(1);
        let mut mapper = Mapper::new(memory).unwrap();
        let root = mapper.root();
        let plan = MappingPlan::new(PhysicalAddress(0x80000000),
                                    VirtualAddress(0xffffffe000000000),
                                    8 << 30,
                                    PageGeometry::new(4096).unwrap(),
                                    Sv39::LEAF_BYTES).unwrap();
        let mut count = 0;
        for mapping in plan
        {
            assert_eq!(mapping.bytes, 1 << 30);
            assert!(mapper.map(mapping.virtual_address.0,
                               mapping.physical,
                               LeafSize::Huge.bytes(),
                               permissions(),
                               PageOwnership::Manual)
                          .unwrap());
            count += 1;
        }
        assert_eq!(count, 8);
        assert_eq!(mapper.tables().frames.len(), 1);
        assert_eq!(mapper.tables().writes, 8);
        assert_eq!(mapper.tables().loads.get(), 8);
        for index in 384..392
        {
            assert!(matches!(Entry::decode_raw(mapper.tables().load(root, index).unwrap(), 2).unwrap(),
                             Decoded::Leaf { size: LeafSize::Huge,
                                             .. }));
        }
    }
    #[test]
    fn small_pages_retry_owned_partial_branches_and_do_not_rewrite_on_no_change()
    {
        let memory = Memory::new(2);
        let mut mapper = Mapper::new(memory).unwrap();
        let error = mapper.map(0x400000,
                               PhysicalAddress(0x90000000),
                               LeafSize::Page.bytes(),
                               permissions(),
                               PageOwnership::Automatic)
                          .unwrap_err();
        assert!(error.to_string().contains("used 2, limit 2"));
        assert_eq!(mapper.tables().frames.len(), 2, "failed preparation discarded an owned branch");
        mapper.tables_mut().limit = 3;
        assert!(mapper.map(0x400000,
                           PhysicalAddress(0x90000000),
                           LeafSize::Page.bytes(),
                           permissions(),
                           PageOwnership::Automatic)
                      .unwrap());
        let writes = mapper.tables().writes;
        assert!(!mapper.map(0x400000,
                            PhysicalAddress(0x90000000),
                            LeafSize::Page.bytes(),
                            permissions(),
                            PageOwnership::Automatic)
                       .unwrap());
        assert_eq!(mapper.tables().writes, writes, "no-change mapping rewrote PTEs");
        assert!(matches!(mapper.map(0x400000,
                                    PhysicalAddress(0x90001000),
                                    LeafSize::Page.bytes(),
                                    permissions(),
                                    PageOwnership::Automatic),
                         Err(MapError::Conflict { .. })));
        assert_eq!(mapper.tables().writes, writes, "collision changed the original mapping");
    }
    #[test]
    fn conflicting_large_leaf_and_foreign_table_are_rejected_without_overwrite()
    {
        let memory = Memory::new(4);
        let mut mapper = Mapper::new(memory).unwrap();
        let root = mapper.root();
        mapper.map(0x40000000,
                   PhysicalAddress(0x80000000),
                   LeafSize::Huge.bytes(),
                   permissions(),
                   PageOwnership::Manual)
              .unwrap();
        let writes = mapper.tables().writes;
        assert!(matches!(mapper.map(0x40000000,
                                    PhysicalAddress(0x80000000),
                                    LeafSize::Page.bytes(),
                                    permissions(),
                                    PageOwnership::Automatic),
                         Err(MapError::Conflict { level: 2,
                                                  .. })));
        assert_eq!(mapper.tables().writes, writes);
        mapper.tables_mut().store(root,0,Entry::table(PhysicalAddress(0xdead000),false,PageOwnership::Manual).unwrap()).unwrap();
        let error = mapper.map(0x400000,
                               PhysicalAddress(0x80000000),
                               LeafSize::Page.bytes(),
                               permissions(),
                               PageOwnership::Manual)
                          .unwrap_err();
        assert!(error.to_string().contains("not owned by this address space"));
        let error = mapper.map(0x400001,
                               PhysicalAddress(0x80000000),
                               LeafSize::Page.bytes(),
                               permissions(),
                               PageOwnership::Manual)
                          .unwrap_err();
        assert!(error.to_string().contains("virtual address 0x400001"));
    }
    #[test]
    fn private_maps_cannot_inherit_global_entries_or_modify_shared_table_backing()
    {
        for (global, ownership) in
            [(true, PageOwnership::Automatic), (false, PageOwnership::CopyOnWrite)]
        {
            let mut mapper = Mapper::new(Memory::new(4)).unwrap();
            let root = mapper.root();
            let child = mapper.tables_mut().allocate_zeroed().unwrap();
            mapper.tables_mut().store(root, 0, Entry::table(child, global, ownership).unwrap()).unwrap();
            let writes = mapper.tables().writes;
            let mut private = permissions();
            private.global = false;
            let error = mapper.map(0x400000,
                                   PhysicalAddress(0x80000000),
                                   LeafSize::Page.bytes(),
                                   private,
                                   PageOwnership::Automatic)
                              .unwrap_err();
            if global
            {
                assert!(matches!(error, MapError::GlobalAncestor { .. }));
                assert!(error.to_string().contains("private subtree"));
            }
            else
            {
                assert!(matches!(error, MapError::CopyOnWriteTable { .. }));
                assert!(error.to_string().contains("private copy"));
            }
            assert_eq!(mapper.tables().writes, writes);
            assert_eq!(mapper.tables().frames.len(), 2);
        }
    }
    #[test]
    fn invalid_entries_with_opaque_software_payload_are_preserved()
    {
        let mut mapper = Mapper::new(Memory::new(4)).unwrap();
        let root = mapper.root();
        // Hardware ignores V=0, but a future pager may own this payload. An
        // ordinary map must refuse rather than silently overwrite that owner.
        mapper.tables_mut().frames.get_mut(&root.0).unwrap()[0] = 0x1234_0000;
        let error = mapper.map(0x400000,
                               PhysicalAddress(0x80000000),
                               LeafSize::Page.bytes(),
                               permissions(),
                               PageOwnership::Automatic)
                          .unwrap_err();
        assert!(matches!(error, MapError::Conflict { .. }));
        assert_eq!(mapper.tables().load(root, 0).unwrap(), 0x1234_0000);
        assert_eq!(mapper.tables().frames.len(), 1);
        assert_eq!(mapper.tables().writes, 0);
    }
}
