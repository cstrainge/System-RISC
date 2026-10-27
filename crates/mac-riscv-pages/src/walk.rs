//! Allocation-free inspection of owned Sv39 tables. Never activates or edits a
//! root. A continuation is a virtual lower bound, not a caller-chosen table ID.
use core::fmt;

use mac_kernel_model::pages::{PageOwnership, Permissions, PhysicalAddress};

use crate::{Decoded, Entry, Error as EncodingError, LeafSize, Sv39, mapping::Tables};

/// Validate ownership once per table and lend a complete value snapshot. Failed
/// reads must not be published as a cached table. Native bindings use volatile
/// reads; this safe interface neither dereferences addresses nor grants access.
pub trait ReadTables
{
    type Error: fmt::Display;
    fn read_table(&self, physical: PhysicalAddress, output: &mut [u64; 512])
                  -> Result<(), Self::Error>;
}
impl<T: Tables> ReadTables for T
{
    type Error = T::Error;

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
pub enum Error<E>
{
    Encoding(EncodingError),
    Storage
    {
        physical: PhysicalAddress,
        cause:    E
    }
}
impl<E: fmt::Display> fmt::Display for Error<E>
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
    pub fn new(source: &'a T, root: PhysicalAddress, start: u64) -> Result<Self, Error<T::Error>>
    {
        Entry::table(root, false, PageOwnership::Manual).map_err(Error::Encoding)?;
        if !Sv39::canonical(start)
        {
            return Err(Error::Encoding(EncodingError::NonCanonical { address: start }));
        }
        Ok(Self { source,
                  root,
                  cache: [CachedTable::empty(), CachedTable::empty(), CachedTable::empty()],
                  next: Some(start),
                  pending: None })
    }

    fn advance(&mut self, base: u64, bytes: u64)
    {
        self.next =
            base.checked_add(bytes).map(|end| if end == 1u64 << 38 { u64::MAX << 38 } else { end });
    }

    fn leaf(&mut self) -> Result<Option<Span>, Error<T::Error>>
    {
        'search: while let Some(virtual_base) = self.next
        {
            let mut table = self.root;
            let mut global = false;
            for level in (0..=2u8).rev()
            {
                let cached = &mut self.cache[usize::from(level)];
                if cached.physical != Some(table)
                {
                    cached.physical = None;
                    self.source.read_table(table,&mut cached.words)
                        .map_err(|cause|Error::Storage{physical:table,cause})?;
                    cached.physical = Some(table);
                }
                let index = Sv39::index(virtual_base, level).map_err(Error::Encoding)?;
                match Entry::decode_raw(cached.words[index], level).map_err(Error::Encoding)?
                {
                    Decoded::Invalid =>
                    {
                        let size = LeafSize::from_level(level).map_err(Error::Encoding)?.bytes();
                        self.advance(virtual_base & !(size - 1), size);
                        continue 'search;
                    }
                    Decoded::Table { physical,
                                     global: parent,
                                     .. } =>
                    {
                        table = physical;
                        global |= parent;
                    }
                    Decoded::Leaf { physical,
                                    size,
                                    mut permissions,
                                    ownership } =>
                    {
                        let offset = virtual_base & (size.bytes() - 1);
                        let bytes = size.bytes() - offset;
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

    pub fn next_span(&mut self) -> Result<Option<Span>, Error<T::Error>>
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

    fn coalesced(&mut self) -> Result<Option<Span>, Error<T::Error>>
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
                LeafSize::from_level(0).unwrap(),
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
