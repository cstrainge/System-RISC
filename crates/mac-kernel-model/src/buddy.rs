//! Physical page bookkeeping. All metadata is supplied by the boot owner and
//! remains outside the pages it describes; allocation/free never allocate.
use core::fmt;

use spinning_top::Spinlock;

use crate::{pages::{PageGeometry, PhysicalAddress},
            policy::{AddressRange, ModelError}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State
{
    Unavailable,
    Interior,
    Free(u32),
    Allocated(u32)
}
#[derive(Clone, Copy, Debug)]
pub struct PageMetadata
{
    state:    State,
    previous: Option<usize>,
    next:     Option<usize>
}
impl PageMetadata
{
    pub const fn unavailable() -> Self
    {
        Self { state:    State::Unavailable,
               previous: None,
               next:     None }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuddyError
{
    Geometry(ModelError),
    Metadata
    {
        pages:           usize,
        required_orders: usize,
        supplied_orders: usize
    },
    InvalidFreeRange,
    AlreadyManaged,
    Exhausted
    {
        requested_order: u32,
        free_pages:      usize
    }
}
impl fmt::Display for BuddyError
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Geometry(e) => e.fmt(f),
            Self::Metadata { pages,
                             required_orders,
                             supplied_orders } =>
            {
                write!(f,
                       "buddy metadata for {pages} pages requires {required_orders} order heads; \
                        received {supplied_orders}")
            }
            Self::InvalidFreeRange =>
            {
                f.write_str("free RAM range must be nonempty, page-aligned, and inside the \
                             physical pool")
            }
            Self::AlreadyManaged =>
            {
                f.write_str("RAM range already belongs to the page pool; reservation cannot be \
                             released twice")
            }
            Self::Exhausted { requested_order,
                              free_pages } =>
            {
                write!(f,
                       "physical page pool cannot satisfy order {requested_order}: {free_pages} \
                        free pages, no suitably aligned contiguous block")
            }
        }
    }
}
pub struct BuddyPool<'storage>
{
    base:     PhysicalAddress,
    geometry: PageGeometry,
    state:    Spinlock<PoolState<'storage>>
}
struct PoolState<'a>
{
    metadata:   &'a mut [PageMetadata],
    heads:      &'a mut [Option<usize>],
    free_pages: usize
}
impl<'storage> BuddyPool<'storage>
{
    pub fn required_orders(pages: usize) -> usize
    {
        if pages == 0 { 0 } else { pages.ilog2() as usize + 1 }
    }

    pub fn new(base: PhysicalAddress, geometry: PageGeometry,
               metadata: &'storage mut [PageMetadata], heads: &'storage mut [Option<usize>])
               -> Result<Self, BuddyError>
    {
        let required = Self::required_orders(metadata.len());
        if heads.len() != required || metadata.is_empty()
        {
            return Err(BuddyError::Metadata { pages:           metadata.len(),
                                              required_orders: required,
                                              supplied_orders: heads.len() });
        }
        let bytes = (metadata.len() as u64).checked_mul(geometry.bytes())
                                           .ok_or(BuddyError::Geometry(ModelError::Overflow))?;
        if !geometry.aligned(base.0) || base.0.checked_add(bytes).is_none()
        {
            return Err(BuddyError::Geometry(ModelError::InvalidGeometry));
        }
        metadata.fill(PageMetadata::unavailable());
        heads.fill(None);
        Ok(Self { base,
                  geometry,
                  state: Spinlock::new(PoolState { metadata,
                                                   heads,
                                                   free_pages: 0 }) })
    }

    /// Bootstrap only: declare disjoint, previously unavailable RAM free. The
    /// caller subtracts the shared BootInfo reservation account first.
    pub fn add_free_range(&self, range: AddressRange) -> Result<(), BuddyError>
    {
        let mut state = self.state.lock();
        let start = range.start().checked_sub(self.base.0).ok_or(BuddyError::InvalidFreeRange)?;
        let end = range.end().checked_sub(self.base.0).ok_or(BuddyError::InvalidFreeRange)?;
        if !self.geometry.aligned(start)
           || !self.geometry.aligned(end)
           || end / self.geometry.bytes() > state.metadata.len() as u64
        {
            return Err(BuddyError::InvalidFreeRange);
        }
        let start = (start / self.geometry.bytes()) as usize;
        let end = (end / self.geometry.bytes()) as usize;
        if state.metadata[start..end].iter().any(|m| m.state != State::Unavailable)
        {
            return Err(BuddyError::AlreadyManaged);
        }
        // One metadata initialization per newly admitted page; no mapping loop.
        for m in &mut state.metadata[start..end]
        {
            m.state = State::Interior;
        }
        state.free_pages += end - start;
        let base_page = self.base.0 / self.geometry.bytes();
        let mut index = start;
        while index < end
        {
            let physical_page = base_page + index as u64;
            let order = (end - index).ilog2()
                                     .min(physical_page.trailing_zeros())
                                     .min((state.heads.len() - 1) as u32);
            state.coalesce(base_page, index, order);
            index += 1usize << order;
        }
        Ok(())
    }

    pub fn allocate(&self, order: u32) -> Result<PageBlock<'_, 'storage>, BuddyError>
    {
        let mut state = self.state.lock();
        let found = (order as usize..state.heads.len()).find(|&n| state.heads[n].is_some())
            .ok_or(BuddyError::Exhausted { requested_order: order, free_pages: state.free_pages })?;
        let index = state.heads[found].unwrap();
        state.unlink(index, found as u32);
        for split in (order as usize..found).rev()
        {
            state.push(index + (1usize << split), split as u32);
        }
        state.metadata[index].state = State::Allocated(order);
        state.free_pages -= 1usize << order;
        Ok(PageBlock { pool: self,
                       index,
                       order })
    }

    pub fn free_pages(&self) -> usize
    {
        self.state.lock().free_pages
    }

    fn release(&self, index: usize, order: u32)
    {
        let mut state = self.state.lock();
        debug_assert_eq!(state.metadata[index].state, State::Allocated(order));
        state.free_pages += 1usize << order;
        state.coalesce(self.base.0 / self.geometry.bytes(), index, order);
    }
}
impl PoolState<'_>
{
    fn push(&mut self, index: usize, order: u32)
    {
        let next = self.heads[order as usize];
        self.metadata[index] = PageMetadata { state: State::Free(order),
                                              previous: None,
                                              next };
        if let Some(next) = next
        {
            self.metadata[next].previous = Some(index);
        }
        self.heads[order as usize] = Some(index);
    }

    fn unlink(&mut self, index: usize, order: u32)
    {
        let entry = self.metadata[index];
        if let Some(previous) = entry.previous
        {
            self.metadata[previous].next = entry.next;
        }
        else
        {
            self.heads[order as usize] = entry.next;
        }
        if let Some(next) = entry.next
        {
            self.metadata[next].previous = entry.previous;
        }
        self.metadata[index] = PageMetadata { state:    State::Interior,
                                              previous: None,
                                              next:     None };
    }

    fn coalesce(&mut self, base_page: u64, mut index: usize, mut order: u32)
    {
        self.metadata[index].state = State::Interior;
        while (order as usize + 1) < self.heads.len()
        {
            let physical_buddy = (base_page + index as u64) ^ (1u64 << order);
            let Some(buddy) =
                physical_buddy.checked_sub(base_page).and_then(|n| usize::try_from(n).ok())
            else
            {
                break;
            };
            if buddy >= self.metadata.len() || self.metadata[buddy].state != State::Free(order)
            {
                break;
            }
            self.unlink(buddy, order);
            index = index.min(buddy);
            order += 1;
        }
        self.push(index, order);
    }
}
/// A unique allocation lease. Safe code cannot forge a block, free it twice,
/// give it to another pool, or outlive its metadata. Drop coalesces in O(orders).
pub struct PageBlock<'pool, 'storage>
{
    pool:  &'pool BuddyPool<'storage>,
    index: usize,
    order: u32
}
impl PageBlock<'_, '_>
{
    pub const fn order(&self) -> u32
    {
        self.order
    }

    pub fn bytes(&self) -> u64
    {
        self.pool.geometry.bytes() * (1u64 << self.order)
    }

    pub fn address(&self) -> PhysicalAddress
    {
        PhysicalAddress(self.pool.base.0 + self.index as u64 * self.pool.geometry.bytes())
    }
}
impl Drop for PageBlock<'_, '_>
{
    fn drop(&mut self)
    {
        self.pool.release(self.index, self.order);
    }
}

#[cfg(test)]
mod tests
{
    use super::*;
    #[test]
    fn split_free_coalesce_and_failure_recovery()
    {
        let mut meta = [PageMetadata::unavailable(); 16];
        let mut heads = [None; 5];
        let pool = BuddyPool::new(PhysicalAddress(0),
                                  PageGeometry::new(4096).unwrap(),
                                  &mut meta,
                                  &mut heads).unwrap();
        pool.add_free_range(AddressRange::new(0, 16 * 4096).unwrap()).unwrap();
        let a = pool.allocate(0).unwrap();
        let b = pool.allocate(0).unwrap();
        assert_ne!(a.address(), b.address());
        let msg = match pool.allocate(4)
        {
            Err(e) => alloc::format!("{e}"),
            Ok(_) => panic!("overlapping allocation")
        };
        assert!(msg.contains("order 4") && msg.contains("14 free pages"));
        drop(a);
        drop(b);
        let all = pool.allocate(4).unwrap();
        assert_eq!(all.bytes(), 65536);
        assert_eq!(pool.free_pages(), 0);
        drop(all);
        assert_eq!(pool.free_pages(), 16);
        assert_eq!(pool.add_free_range(AddressRange::new(0, 4096).unwrap()),
                   Err(BuddyError::AlreadyManaged));
    }
    #[test]
    fn physical_alignment_reservations_and_other_page_geometry()
    {
        let page = 65536;
        let base = 3 * page;
        let mut meta = [PageMetadata::unavailable(); 13];
        let mut heads = [None; 4];
        let pool = BuddyPool::new(PhysicalAddress(base),
                                  PageGeometry::new(page).unwrap(),
                                  &mut meta,
                                  &mut heads).unwrap();
        pool.add_free_range(AddressRange::new(base, base + 3 * page).unwrap()).unwrap();
        // One reserved page remains unavailable between the admitted spans.
        pool.add_free_range(AddressRange::new(base + 4 * page, base + 13 * page).unwrap()).unwrap();
        assert_eq!(pool.free_pages(), 12);
        let high = pool.allocate(3).unwrap();
        assert_eq!(high.address().0 % (page * 8), 0);
        let mut singles = alloc::vec::Vec::new();
        while let Ok(block) = pool.allocate(0)
        {
            assert_ne!(block.address().0, base + 3 * page);
            singles.push(block);
        }
        assert_eq!(singles.len(), 4);
        drop(singles);
        drop(high);
        assert_eq!(pool.free_pages(), 12);
        assert!(matches!(pool.allocate(u32::MAX), Err(BuddyError::Exhausted { .. })));
    }
    #[test]
    fn boot_metadata_and_admission_fail_before_mutation()
    {
        let mut meta = [PageMetadata::unavailable(); 3];
        let mut short = [None; 1];
        assert!(matches!(BuddyPool::new(PhysicalAddress(0),
                                        PageGeometry::new(4096).unwrap(),
                                        &mut meta,
                                        &mut short),
                         Err(BuddyError::Metadata { required_orders: 2,
                                                    .. })));
        let mut heads = [None; 2];
        let pool = BuddyPool::new(PhysicalAddress(0),
                                  PageGeometry::new(4096).unwrap(),
                                  &mut meta,
                                  &mut heads).unwrap();
        assert_eq!(pool.add_free_range(AddressRange::new(1, 4096).unwrap()),
                   Err(BuddyError::InvalidFreeRange));
        assert_eq!(pool.free_pages(), 0);
        pool.add_free_range(AddressRange::new(0, 8192).unwrap()).unwrap();
        assert_eq!(pool.add_free_range(AddressRange::new(4096, 12288).unwrap()),
                   Err(BuddyError::AlreadyManaged));
        assert_eq!(pool.free_pages(), 2);
        pool.add_free_range(AddressRange::new(8192, 12288).unwrap()).unwrap();
        assert_eq!(pool.free_pages(), 3);
    }
}
