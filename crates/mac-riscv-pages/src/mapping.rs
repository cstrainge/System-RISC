//! Safe hierarchy construction over a caller's owned, checked table storage.
//! Storage owns every allocated frame through success and refusal. This layer
//! never dereferences an address, activates a root or acknowledges reclamation.
use core::fmt;

use mac_kernel_model::{pages::{MappingPlan, PageLayout, PageOwnership, Permissions,
                               PhysicalAddress, VirtualAddress},
                       policy::{AddressRange, ModelError}};

use crate::{Decoded, Entry, Error, LeafSize, Sv39};

/// The architecture binding validates frame ownership and slice access once.
/// Allocations are zeroed and retained by this owner until its own RAII teardown;
/// a partially prepared root never leaves frames for a caller to remember.
/// `store` validates before publication: Err leaves the addressed entry intact.
/// These contracts cover typed refusals; arbitrary privileged-fault recovery
/// remains an unproven gate in the native binding.
pub trait Tables
{
    type Error: fmt::Display;
    fn allocate_zeroed(&mut self) -> Result<PhysicalAddress, Self::Error>;
    fn load(&self, table: PhysicalAddress, index: usize) -> Result<u64, Self::Error>;
    fn store(&mut self, table: PhysicalAddress, index: usize, entry: Entry)
             -> Result<(), Self::Error>;
}
#[derive(Debug)]
pub enum MapError<E>
{
    Encoding(Error),
    Storage(E),
    Span
    {
        virtual_address: u64,
        physical:        u64,
        bytes:           u64,
        cause:           ModelError
    },
    NonCanonicalSpan
    {
        virtual_address: u64,
        bytes:           u64
    },
    GlobalAncestor
    {
        virtual_address: u64,
        level:           u8
    },
    CopyOnWriteTable
    {
        virtual_address: u64,
        level:           u8
    },
    Conflict
    {
        virtual_address: u64,
        level:           u8,
        existing:        u64,
        requested:       u64
    }
}
impl<E: fmt::Display> fmt::Display for MapError<E>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Encoding(error) => error.fmt(f),
            Self::Span { virtual_address,
                         physical,
                         bytes,
                         cause } =>
            {
                write!(f,
                       "mapping VA {virtual_address:#x} to PA {physical:#x} for {bytes:#x} bytes \
                        refused: {cause}")
            }
            Self::Storage(error) => write!(f, "page-table storage refused mapping: {error}"),
            Self::NonCanonicalSpan { virtual_address,
                                     bytes } =>
            {
                write!(f,
                       "mapping VA {virtual_address:#x}+{bytes:#x} crosses the Sv39 canonical \
                        hole or lies outside a canonical half")
            }
            Self::GlobalAncestor { virtual_address,
                                   level } =>
            {
                write!(f,
                       "private mapping {virtual_address:#x} would inherit a global Sv39 ancestor \
                        at level {level}; use a private subtree")
            }
            Self::CopyOnWriteTable { virtual_address,
                                     level } =>
            {
                write!(f,
                       "mapping {virtual_address:#x} requires a private copy of its level {level} \
                        page table; copy-on-write table mutation is not implemented")
            }
            Self::Conflict { virtual_address,
                             level,
                             existing,
                             requested } =>
            {
                write!(f,
                       "mapping {virtual_address:#x} conflicts at Sv39 level {level}: existing \
                        PTE {existing:#x}, requested {requested:#x}; existing mapping is unchanged")
            }
        }
    }
}
/// Preparation has exclusive access to its table owner. Only the native binding
/// can later turn its actual root lease into a CPU activation proof.
pub struct Mapper<T: Tables>
{
    tables: T,
    root:   PhysicalAddress
}
impl<T: Tables> Mapper<T>
{
    pub fn new(mut tables: T) -> Result<Self, MapError<T::Error>>
    {
        let root = tables.allocate_zeroed().map_err(MapError::Storage)?;
        Entry::table(root, false, PageOwnership::Automatic).map_err(MapError::Encoding)?;
        Ok(Self { tables,
                  root })
    }

    pub fn root(&self) -> PhysicalAddress
    {
        self.root
    }

    /// Transfer the actual retained root and storage into the architecture's
    /// owned address space. This neither activates nor duplicates ownership.
    pub fn into_tables(self) -> (PhysicalAddress, T)
    {
        (self.root, self.tables)
    }

    /// Populate an extent using the largest aligned leaves. Invalid extents
    /// refuse before the first table access; later storage/collision refusal
    /// keeps the same owned partial preparation, never discarding its frames.
    /// Return (leaves visited, entries changed); a repeated range writes none.
    pub fn map_range(&mut self, virtual_address: u64, physical: PhysicalAddress, bytes: u64,
                     permissions: Permissions, ownership: PageOwnership)
                     -> Result<(u64, u64), MapError<T::Error>>
    {
        if bytes == 0
        {
            return Ok((0, 0));
        }
        let span = |cause| {
            MapError::Span { virtual_address,
                             physical: physical.0,
                             bytes,
                             cause }
        };
        let end = virtual_address.checked_add(bytes).ok_or_else(|| span(ModelError::Overflow))?;
        let range = AddressRange::new(virtual_address, end).map_err(span)?;
        if !Sv39::canonical_range(range)
        {
            return Err(MapError::NonCanonicalSpan { virtual_address,
                                                    bytes });
        }
        let plan = MappingPlan::new(physical,
                                    VirtualAddress(virtual_address),
                                    bytes,
                                    Sv39::geometry(),
                                    Sv39::LEAF_BYTES).map_err(span)?;
        let mut leaves = 0;
        let mut changed = 0;
        for mapping in plan
        {
            let size = match mapping.bytes
            {
                0x40000000 => LeafSize::Huge,
                0x200000 => LeafSize::Large,
                0x1000 => LeafSize::Page,
                _ => return Err(span(ModelError::InvalidGeometry))
            };
            changed += u64::from(self.map(mapping.virtual_address.0, mapping.physical, size,
                                          permissions, ownership)?);
            leaves += 1;
        }
        Ok((leaves, changed))
    }

    /// Return whether a leaf changed. Identical mappings write nothing; a
    /// collision never overwrites an existing leaf or an occupied subtree.
    /// Allocation refusal may leave empty owned branches for a later retry.
    pub fn map(&mut self, virtual_address: u64, physical: PhysicalAddress, size: LeafSize,
               permissions: Permissions, ownership: PageOwnership)
               -> Result<bool, MapError<T::Error>>
    {
        let requested =
            Entry::leaf(physical, size, permissions, ownership).map_err(MapError::Encoding)?;
        Sv39::index(virtual_address, 2).map_err(MapError::Encoding)?;
        if !virtual_address.is_multiple_of(size.bytes())
        {
            return Err(MapError::Encoding(Error::VirtualAlignment { address:   virtual_address,
                                                                    alignment: size.bytes() }));
        }
        // A leaf may not cross into a noncanonical hole or wrap the address.
        let end=virtual_address.checked_add(size.bytes()-1)
            .filter(|end|Sv39::canonical(*end)).ok_or(MapError::Encoding(Error::NonCanonical{address:virtual_address}))?;
        if virtual_address >> 38 != end >> 38
        {
            return Err(MapError::Encoding(Error::NonCanonical { address: end }));
        }
        let mut table = self.root;
        for level in (size.level()..=2).rev()
        {
            let index = Sv39::index(virtual_address, level).map_err(MapError::Encoding)?;
            let bits = self.tables.load(table, index).map_err(MapError::Storage)?;
            if level == size.level()
            {
                if bits == requested.bits()
                {
                    return Ok(false);
                }
                Entry::decode_raw(bits, level).map_err(MapError::Encoding)?;
                if bits == 0
                {
                    self.tables.store(table, index, requested).map_err(MapError::Storage)?;
                    return Ok(true);
                }
                return Err(MapError::Conflict { virtual_address,
                                                level,
                                                existing: bits,
                                                requested: requested.bits() });
            }
            match Entry::decode_raw(bits, level).map_err(MapError::Encoding)?
            {
                Decoded::Table { physical,
                                 global,
                                 ownership } =>
                {
                    if global && !permissions.global
                    {
                        return Err(MapError::GlobalAncestor { virtual_address,
                                                              level });
                    }
                    if ownership == PageOwnership::CopyOnWrite
                    {
                        return Err(MapError::CopyOnWriteTable { virtual_address,
                                                                level });
                    }
                    table = physical;
                }
                Decoded::Invalid if bits == 0 =>
                {
                    let next = self.tables.allocate_zeroed().map_err(MapError::Storage)?;
                    let branch=Entry::table(next,false,PageOwnership::Automatic).map_err(MapError::Encoding)?;
                    self.tables.store(table, index, branch).map_err(MapError::Storage)?;
                    table = next;
                }
                Decoded::Leaf { .. } | Decoded::Invalid =>
                {
                    return Err(MapError::Conflict { virtual_address,
                                                    level,
                                                    existing: bits,
                                                    requested: requested.bits() });
                }
            }
        }
        // Every legal Sv39 level has returned after installing/validating its
        // target; this fallible case reports a broken geometry contract.
        Err(MapError::Encoding(Error::Level { level: size.level() }))
    }
}
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
        assert_eq!(mapper.tables.frames.len(), 5);
        let writes = mapper.tables.writes;
        assert_eq!(mapper.map_range(base + 0x3ffff000,
                                    PhysicalAddress(0x3ffff000),
                                    (1 << 30) + 8192,
                                    permissions(),
                                    PageOwnership::Manual)
                         .unwrap(),
                   (3, 0));
        assert_eq!(mapper.tables.writes, writes);
        let loads = mapper.tables.loads.get();
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
        assert_eq!(mapper.tables.loads.get(), loads, "invalid range touched a table");
        assert_eq!(mapper.tables.writes, writes);
        assert_eq!(mapper.map_range(base,
                                    PhysicalAddress(0),
                                    0,
                                    permissions(),
                                    PageOwnership::Manual)
                         .unwrap(),
                   (0, 0));
        assert_eq!(mapper.tables.loads.get(), loads, "empty range did unnecessary work");
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
                               LeafSize::Huge,
                               permissions(),
                               PageOwnership::Manual)
                          .unwrap());
            count += 1;
        }
        assert_eq!(count, 8);
        assert_eq!(mapper.tables.frames.len(), 1);
        assert_eq!(mapper.tables.writes, 8);
        assert_eq!(mapper.tables.loads.get(), 8);
        for index in 384..392
        {
            assert!(matches!(Entry::decode_raw(mapper.tables.load(root, index).unwrap(), 2).unwrap(),
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
                               LeafSize::Page,
                               permissions(),
                               PageOwnership::Automatic)
                          .unwrap_err();
        assert!(error.to_string().contains("used 2, limit 2"));
        assert_eq!(mapper.tables.frames.len(), 2, "failed preparation discarded an owned branch");
        mapper.tables.limit = 3;
        assert!(mapper.map(0x400000,
                           PhysicalAddress(0x90000000),
                           LeafSize::Page,
                           permissions(),
                           PageOwnership::Automatic)
                      .unwrap());
        let writes = mapper.tables.writes;
        assert!(!mapper.map(0x400000,
                            PhysicalAddress(0x90000000),
                            LeafSize::Page,
                            permissions(),
                            PageOwnership::Automatic)
                       .unwrap());
        assert_eq!(mapper.tables.writes, writes, "no-change mapping rewrote PTEs");
        assert!(matches!(mapper.map(0x400000,
                                    PhysicalAddress(0x90001000),
                                    LeafSize::Page,
                                    permissions(),
                                    PageOwnership::Automatic),
                         Err(MapError::Conflict { .. })));
        assert_eq!(mapper.tables.writes, writes, "collision changed the original mapping");
    }
    #[test]
    fn conflicting_large_leaf_and_foreign_table_are_rejected_without_overwrite()
    {
        let memory = Memory::new(4);
        let mut mapper = Mapper::new(memory).unwrap();
        let root = mapper.root();
        mapper.map(0x40000000,
                   PhysicalAddress(0x80000000),
                   LeafSize::Huge,
                   permissions(),
                   PageOwnership::Manual)
              .unwrap();
        let writes = mapper.tables.writes;
        assert!(matches!(mapper.map(0x40000000,
                                    PhysicalAddress(0x80000000),
                                    LeafSize::Page,
                                    permissions(),
                                    PageOwnership::Automatic),
                         Err(MapError::Conflict { level: 2,
                                                  .. })));
        assert_eq!(mapper.tables.writes, writes);
        mapper.tables.store(root,0,Entry::table(PhysicalAddress(0xdead000),false,PageOwnership::Manual).unwrap()).unwrap();
        let error = mapper.map(0x400000,
                               PhysicalAddress(0x80000000),
                               LeafSize::Page,
                               permissions(),
                               PageOwnership::Manual)
                          .unwrap_err();
        assert!(error.to_string().contains("not owned by this address space"));
        let error = mapper.map(0x400001,
                               PhysicalAddress(0x80000000),
                               LeafSize::Page,
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
            let child = mapper.tables.allocate_zeroed().unwrap();
            mapper.tables.store(root, 0, Entry::table(child, global, ownership).unwrap()).unwrap();
            let writes = mapper.tables.writes;
            let mut private = permissions();
            private.global = false;
            let error = mapper.map(0x400000,
                                   PhysicalAddress(0x80000000),
                                   LeafSize::Page,
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
            assert_eq!(mapper.tables.writes, writes);
            assert_eq!(mapper.tables.frames.len(), 2);
        }
    }
    #[test]
    fn invalid_entries_with_opaque_software_payload_are_preserved()
    {
        let mut mapper = Mapper::new(Memory::new(4)).unwrap();
        let root = mapper.root();
        // Hardware ignores V=0, but a future pager may own this payload. An
        // ordinary map must refuse rather than silently overwrite that owner.
        mapper.tables.frames.get_mut(&root.0).unwrap()[0] = 0x1234_0000;
        let error = mapper.map(0x400000,
                               PhysicalAddress(0x80000000),
                               LeafSize::Page,
                               permissions(),
                               PageOwnership::Automatic)
                          .unwrap_err();
        assert!(matches!(error, MapError::Conflict { .. }));
        assert_eq!(mapper.tables.load(root, 0).unwrap(), 0x1234_0000);
        assert_eq!(mapper.tables.frames.len(), 1);
        assert_eq!(mapper.tables.writes, 0);
    }
}
