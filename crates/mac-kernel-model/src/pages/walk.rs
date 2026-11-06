//! Allocation-free inspection of owned page tables. Never activates or edits a
//! root. A continuation is a virtual lower bound, not a caller-chosen table ID.
//!
//! The walk is written once. What an entry means is asked of the descriptor
//! type and how an address selects an index is asked of the layout type, so this
//! code contains no architecture's encodings.
use core::fmt;

use super::{Descriptor, EntryKind, PageLayout, PageOwnership, Permissions, PhysicalAddress,
            mapping};

/// Validate ownership once per table and lend a complete value snapshot. Failed
/// reads must not be published as a cached table. Native bindings use volatile
/// reads; this safe interface neither dereferences addresses nor grants access.
pub trait ReadTables
{
    type Error: fmt::Display;
    type Descriptor: Descriptor;
    type Layout: PageLayout;
    fn read_table(&self, physical: PhysicalAddress, output: &mut [u64; 512])
                  -> Result<(), Self::Error>;
}

impl<T: mapping::Tables> ReadTables for T
{
    type Error = T::Error;
    type Descriptor = T::Descriptor;
    type Layout = T::Layout;

    fn read_table(&self, physical: PhysicalAddress, output: &mut [u64; 512])
                  -> Result<(), Self::Error>
    {
        for (index, value) in output.iter_mut().enumerate()
        {
            *value = self.load(physical, index)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span
{
    pub virtual_base:  u64,
    pub physical_base: PhysicalAddress,
    pub bytes:         u64,
    pub permissions:   Permissions,
    pub ownership:     PageOwnership
}

#[derive(Debug)]
pub enum Error<E, F>
{
    Encoding(F),
    Storage
    {
        physical: PhysicalAddress,
        cause:    E
    },
    /// The starting address is not canonical for this layout.
    NonCanonical
    {
        address: u64
    },
    /// The layout does not describe the level being walked. That is a broken
    /// geometry contract, not a bad address.
    Geometry
    {
        level: usize
    }
}

impl<E: fmt::Display, F: fmt::Display> fmt::Display for Error<E, F>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Encoding(e) => e.fmt(f),
            Self::Storage { physical,
                            cause } =>
            {
                write!(f, "cannot inspect owned page table {:#x}: {cause}", physical.0)
            }
            Self::NonCanonical { address } => write!(f, "non-canonical start {address:#x}"),
            Self::Geometry { level } =>
            {
                write!(f, "the layout does not describe level {level}")
            }
        }
    }
}

struct CachedTable
{
    physical: Option<PhysicalAddress>,
    words:    [u64; 512]
}

impl CachedTable
{
    fn empty() -> Self
    {
        Self { physical: None,
               words:    [0; 512] }
    }
}

/// Bounded three-page scratch, independent of RAM size or number of mappings.
/// Reuses table snapshots while advancing, and coalesces only physically and
/// virtually adjacent leaves with identical protection/ownership. Unmapped
/// holes stay holes. The source retains backing and supplies snapshot stability.
pub struct Walker<'a, T: ReadTables>
{
    source:  &'a T,
    root:    PhysicalAddress,
    cache:   [CachedTable; 3],
    next:    Option<u64>,
    pending: Option<Span>
}

impl<'a, T: ReadTables> Walker<'a, T>
{
    pub fn new(source: &'a T, root: PhysicalAddress, start: u64)
               -> Result<Self, Error<T::Error, <T::Descriptor as Descriptor>::Error>>
    {
        T::Descriptor::table(root, false, PageOwnership::Manual).map_err(Error::Encoding)?;
        if !T::Layout::canonical(start)
        {
            return Err(Error::NonCanonical { address: start });
        }
        Ok(Self { source,
                  root,
                  cache: [CachedTable::empty(), CachedTable::empty(), CachedTable::empty()],
                  next: Some(start),
                  pending: None })
    }

    fn advance(&mut self, base: u64, bytes: u64)
    {
        self.next = base.checked_add(bytes).map(|end| {
                                                   if end == T::Layout::lower_half_end()
                                                   {
                                                       T::Layout::upper_half_start()
                                                   }
                                                   else
                                                   {
                                                       end
                                                   }
                                               });
    }

    fn leaf(&mut self) -> Result<Option<Span>, Error<T::Error, <T::Descriptor as Descriptor>::Error>>
    {
        'search: while let Some(virtual_base) = self.next
        {
            let mut table = self.root;
            let mut global = false;
            for level in (0..T::Layout::levels()).rev()
            {
                let cached = &mut self.cache[level];
                if cached.physical != Some(table)
                {
                    cached.physical = None;
                    self.source.read_table(table,&mut cached.words)
                        .map_err(|cause|Error::Storage{physical:table,cause})?;
                    cached.physical = Some(table);
                }
                let leaf_bytes = T::Layout::leaf_at(level).ok_or(Error::Geometry { level })?;
                let index = T::Layout::index(virtual_base, leaf_bytes)
                    .ok_or(Error::Geometry { level })?;
                match T::Descriptor::decode(cached.words[index], level as u8)
                    .map_err(Error::Encoding)?
                {
                    EntryKind::Invalid =>
                    {
                        self.advance(virtual_base & !(leaf_bytes - 1), leaf_bytes);
                        continue 'search;
                    }
                    EntryKind::Table { physical,
                                       global: parent,
                                       .. } =>
                    {
                        table = physical;
                        global |= parent;
                    }
                    EntryKind::Leaf { physical,
                                      size_bytes,
                                      mut permissions,
                                      ownership } =>
                    {
                        let offset = virtual_base & (size_bytes - 1);
                        let bytes = size_bytes - offset;
                        permissions.global |= global;
                        self.advance(virtual_base, bytes);
                        return Ok(Some(Span { virtual_base,
                                              physical_base: PhysicalAddress(physical.0
                                                                             + offset),
                                              bytes,
                                              permissions,
                                              ownership }));
                    }
                }
            }
        }
        Ok(None)
    }

    pub fn next_span(&mut self)
                     -> Result<Option<Span>, Error<T::Error, <T::Descriptor as Descriptor>::Error>>
    {
        let position = (self.next, self.pending);
        let result = self.coalesced();
        if result.is_err()
        {
            self.next = position.0;
            self.pending = position.1;
        }
        result
    }

    fn coalesced(&mut self)
                 -> Result<Option<Span>, Error<T::Error, <T::Descriptor as Descriptor>::Error>>
    {
        let first = match self.pending.take()
        {
            Some(span) => Some(span),
            None => self.leaf()?
        };
        let Some(mut span) = first
        else
        {
            return Ok(None);
        };
        while let Some(next) = self.leaf()?
        {
            if span.virtual_base.checked_add(span.bytes) == Some(next.virtual_base)
               && span.physical_base.0.checked_add(span.bytes) == Some(next.physical_base.0)
               && span.permissions == next.permissions
               && span.ownership == next.ownership
            {
                if let Some(bytes) = span.bytes.checked_add(next.bytes)
                {
                    span.bytes = bytes;
                    continue;
                }
            }
            self.pending = Some(next);
            break;
        }
        Ok(Some(span))
    }
}
