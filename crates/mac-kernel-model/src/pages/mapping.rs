//! Safe hierarchy construction over a caller's owned, checked table storage.
//!
//! Storage owns every allocated frame through success and refusal. This layer
//! never dereferences an address, activates a root or acknowledges reclamation.
//!
//! The walk is written once. What an entry means is asked of the descriptor
//! type, and how an address selects a table index is asked of the layout type,
//! so this code contains no architecture's encodings.
use core::fmt;

use super::{Descriptor, EntryKind, MappingPlan, PageLayout, PageOwnership, Permissions,
            PhysicalAddress, VirtualAddress};
use crate::policy::{AddressRange, ModelError};

/// The architecture binding validates frame ownership and slice access once.
/// Allocations are zeroed and retained by this owner until its own RAII teardown;
/// a partially prepared root never leaves frames for a caller to remember.
/// `store` validates before publication: Err leaves the addressed entry intact.
/// These contracts cover typed refusals; arbitrary privileged-fault recovery
/// remains an unproven gate in the native binding.
pub trait Tables
{
    type Error: fmt::Display;
    type Descriptor: Descriptor;
    type Layout: PageLayout;
    fn allocate_zeroed(&mut self) -> Result<PhysicalAddress, Self::Error>;
    fn load(&self, table: PhysicalAddress, index: usize) -> Result<u64, Self::Error>;
    fn store(&mut self, table: PhysicalAddress, index: usize, entry: Self::Descriptor)
             -> Result<(), Self::Error>;
}

#[derive(Debug)]
pub enum MapError<E, F>
{
    Encoding(F),
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
    },
    /// The address is not aligned to the leaf it asks for.
    Misaligned
    {
        virtual_address: u64,
        alignment:       u64
    },
    /// The layout and the request disagree about the leaf sizes this format
    /// has. That is a broken geometry contract, not a bad address.
    Geometry
    {
        virtual_address: u64,
        size_bytes:      u64
    }
}

impl<E: fmt::Display, F: fmt::Display> fmt::Display for MapError<E, F>
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
                       "mapping VA {virtual_address:#x}+{bytes:#x} crosses the canonical hole or \
                        lies outside a canonical half")
            }
            Self::GlobalAncestor { virtual_address,
                                   level } =>
            {
                write!(f,
                       "private mapping {virtual_address:#x} would inherit a global ancestor at \
                        level {level}; use a private subtree")
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
                       "mapping {virtual_address:#x} conflicts at level {level}: existing entry \
                        {existing:#x}, requested {requested:#x}; existing mapping is unchanged")
            }
            Self::Misaligned { virtual_address,
                               alignment } =>
            {
                write!(f, "virtual address {virtual_address:#x} is not {alignment:#x} aligned")
            }
            Self::Geometry { virtual_address,
                             size_bytes } =>
            {
                write!(f,
                       "mapping {virtual_address:#x} requests a {size_bytes:#x}-byte leaf this \
                        layout does not declare")
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
    pub fn new(mut tables: T) -> Result<Self, MapError<T::Error, <T::Descriptor as Descriptor>::Error>>
    {
        let root = tables.allocate_zeroed().map_err(MapError::Storage)?;
        T::Descriptor::table(root, false, PageOwnership::Automatic)
            .map_err(MapError::Encoding)?;
        Ok(Self { tables,
                  root })
    }

    pub fn root(&self) -> PhysicalAddress
    {
        self.root
    }

    /// The owned storage, for inspection and for callers that must read what a
    /// preparation actually allocated.
    pub fn tables(&self) -> &T
    {
        &self.tables
    }

    /// Mutable storage, for a caller that must seed or inspect a preparation.
    pub fn tables_mut(&mut self) -> &mut T
    {
        &mut self.tables
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
                     -> Result<(u64, u64), MapError<T::Error, <T::Descriptor as Descriptor>::Error>>
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
        if !T::Layout::valid_virtual_range(range)
        {
            return Err(MapError::NonCanonicalSpan { virtual_address,
                                                    bytes });
        }
        let plan = MappingPlan::new(physical,
                                    VirtualAddress(virtual_address),
                                    bytes,
                                    T::Layout::geometry(),
                                    T::Layout::leaf_sizes()).map_err(span)?;
        let mut leaves = 0;
        let mut changed = 0;
        for mapping in plan
        {
            // The plan only emits leaves this layout declares, so an unknown
            // size here would mean the geometry changed under us.
            if !T::Layout::leaf_sizes().contains(&mapping.bytes)
            {
                return Err(span(ModelError::InvalidGeometry));
            }
            changed += u64::from(self.map(mapping.virtual_address.0,
                                          mapping.physical,
                                          mapping.bytes,
                                          permissions,
                                          ownership)?);
            leaves += 1;
        }
        Ok((leaves, changed))
    }

    /// Return whether a leaf changed. Identical mappings write nothing; a
    /// collision never overwrites an existing leaf or an occupied subtree.
    /// Allocation refusal may leave empty owned branches for a later retry.
    pub fn map(&mut self, virtual_address: u64, physical: PhysicalAddress, size_bytes: u64,
               permissions: Permissions, ownership: PageOwnership)
               -> Result<bool, MapError<T::Error, <T::Descriptor as Descriptor>::Error>>
    {
        // Find the level by asking what each level maps, not by indexing the
        // declared leaf list: formats may list their leaves in either order.
        let Some(level) = (0..T::Layout::levels()).find(|candidate| {
                                                       T::Layout::leaf_at(*candidate)
                                                       == Some(size_bytes)
                                                   })
        else
        {
            return Err(MapError::Geometry { virtual_address,
                                            size_bytes });
        };
        let level = level as u8;
        let geometry = |address: u64, size: u64| MapError::Geometry { virtual_address: address,
                                                                      size_bytes:      size };
        let requested = T::Descriptor::leaf(physical, size_bytes, permissions, ownership)
            .map_err(MapError::Encoding)?;
        if !virtual_address.is_multiple_of(size_bytes)
        {
            return Err(MapError::Misaligned { virtual_address,
                                              alignment: size_bytes });
        }
        let top = T::Layout::levels() - 1;
        let mut table = self.root;
        for current in (usize::from(level)..=top).rev()
        {
            let leaf = T::Layout::leaf_at(current).ok_or_else(|| geometry(virtual_address,
                                                                          size_bytes))?;
            let index = T::Layout::index(virtual_address, leaf)
                .ok_or_else(|| geometry(virtual_address, leaf))?;
            let bits = self.tables.load(table, index).map_err(MapError::Storage)?;
            if current == usize::from(level)
            {
                if bits == requested.bits()
                {
                    return Ok(false);
                }
                T::Descriptor::decode(bits, level).map_err(MapError::Encoding)?;
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
            match T::Descriptor::decode(bits, current as u8).map_err(MapError::Encoding)?
            {
                EntryKind::Table { physical,
                                   global,
                                   ownership } =>
                {
                    if global && !permissions.global
                    {
                        return Err(MapError::GlobalAncestor { virtual_address,
                                                              level: current as u8 });
                    }
                    if ownership == PageOwnership::CopyOnWrite
                    {
                        return Err(MapError::CopyOnWriteTable { virtual_address,
                                                                level: current as u8 });
                    }
                    table = physical;
                }
                EntryKind::Invalid if bits == 0 =>
                {
                    let next = self.tables.allocate_zeroed().map_err(MapError::Storage)?;
                    let branch = T::Descriptor::table(next, false, PageOwnership::Automatic)
                        .map_err(MapError::Encoding)?;
                    self.tables.store(table, index, branch).map_err(MapError::Storage)?;
                    table = next;
                }
                EntryKind::Leaf { .. } | EntryKind::Invalid =>
                {
                    return Err(MapError::Conflict { virtual_address,
                                                    level: current as u8,
                                                    existing: bits,
                                                    requested: requested.bits() });
                }
            }
        }
        Err(geometry(virtual_address, size_bytes))
    }
}
