//! Sealed reusable backing inside the protected heap tail. This storage never
//! aliases the terminal kit, obtains another page lease or falls back to the
//! ordinary heap. Service claims/lifetimes belong to the recovery coordinator;
//! persistent buffers may survive many claims without resetting the free list.
use core::{alloc::Layout,
           fmt,
           ptr::{self, NonNull},
           sync::atomic::{AtomicBool, Ordering}};

use linked_list_allocator::{LockedHeap, hole::HoleList};
use mac_kernel_model::pages::PhysicalAddress;
use spinning_top::Spinlock;

use super::{SystemClass, budget};
use crate::machine::{interrupts::with_masked, paging::ProtectedRange};

#[derive(Debug)]
pub(crate) enum Error
{
    AlreadyInitialized,
    Layout,
    Exhausted
    {
        used:      u64,
        capacity:  u64,
        requested: u64
    },
    BackingRefused
    {
        requested: usize,
        alignment: usize,
        free:      usize
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::AlreadyInitialized =>
            {
                out.write_str("protected recovery storage authority was already issued")
            }
            Self::Layout => out.write_str("protected recovery backing layout is invalid"),
            Self::Exhausted { used,
                              capacity,
                              requested } =>
            {
                write!(out,
                       "protected recovery storage refused {requested} bytes: {used} bytes \
                        admitted, {capacity} byte capacity")
            }
            Self::BackingRefused { requested,
                                   alignment,
                                   free } =>
            {
                write!(out,
                       "protected recovery free list refused {requested} bytes aligned to \
                        {alignment} with {free} bytes free")
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Snapshot
{
    pub(crate) capacity:    u64,
    pub(crate) admitted:    u64,
    pub(crate) foundation:  u64,
    pub(crate) system:      u64,
    pub(crate) domains:     u64,
    pub(crate) raw_backing: u64
}
#[derive(Clone, Copy)]
enum Charge
{
    Allocation(budget::Category),
    RawBacking
}
impl Snapshot
{
    fn counter(&mut self, charge: Charge) -> &mut u64
    {
        match charge
        {
            Charge::Allocation(budget::Category::System(SystemClass::Foundation)) =>
            {
                &mut self.foundation
            }
            Charge::Allocation(budget::Category::System(SystemClass::Ordinary)) => &mut self.system,
            Charge::Allocation(budget::Category::Domain) => &mut self.domains,
            Charge::RawBacking => &mut self.raw_backing
        }
    }
}
struct Storage
{
    range:    ProtectedRange,
    heap:     LockedHeap,
    counters: Spinlock<Snapshot>
}
static STORAGE: spin::Once<Storage> = spin::Once::new();
static CLAIMED: AtomicBool = AtomicBool::new(false);

/// The sole storage authority is kept by the resident recovery coordinator.
/// This is not a modal claim, application flag or independently owned page pool.
pub(crate) struct Authority
{
    storage: &'static Storage
}
/// An allocation-free storage capability. It carries no Account/Owner Arc and
/// cannot be constructed from an observed domain or service identifier.
#[derive(Clone)]
pub(crate) struct Permit
{
    storage: &'static Storage
}
impl Authority
{
    pub(crate) fn permit(&self) -> Permit
    {
        Permit { storage: self.storage }
    }
}

/// Consume the uniquely handed-off protected tail after terminal preparation.
/// Storage stays mapped/owned until reset. A failed setup never borrows terminal
/// bytes or silently publishes an ordinary-heap substitute.
pub(crate) fn initialize(range: ProtectedRange) -> Result<Authority, Error>
{
    CLAIMED.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
           .map_err(|_| Error::AlreadyInitialized)?;
    let storage = STORAGE.try_call_once(|| Storage::new(range))?;
    Ok(Authority { storage })
}
pub(crate) fn statistics() -> Option<Snapshot>
{
    let storage = STORAGE.get()?;
    Some(with_masked(|| *storage.counters.lock()))
}
impl Storage
{
    fn new(range: ProtectedRange) -> Result<Self, Error>
    {
        let length = usize::try_from(range.bytes()).map_err(|_| Error::Layout)?;
        if length == 0 || range.cpu_base().checked_add(length).is_none()
        {
            return Err(Error::Layout);
        }
        let heap = LockedHeap::empty();
        let capacity = with_masked(|| {
            let mut heap = heap.lock();
            // SAFETY: the moved capability uniquely owns this initialized,
            // permanently mapped tail, disjoint from ordinary/terminal storage.
            unsafe { heap.init(range.cpu_base() as *mut u8, length) };
            heap.size() as u64
        });
        Ok(Self { range,
                  heap,
                  counters: Spinlock::new(Snapshot { capacity,
                                                     admitted: 0,
                                                     foundation: 0,
                                                     system: 0,
                                                     domains: 0,
                                                     raw_backing: 0 }) })
    }

    fn reserve(&self, charge: Charge, bytes: u64) -> Result<(), Error>
    {
        with_masked(|| {
            let mut counters = self.counters.lock();
            let next = counters.admitted
                               .checked_add(bytes)
                               .filter(|&n| n <= counters.capacity)
                               .ok_or(Error::Exhausted { used:      counters.admitted,
                                                         capacity:  counters.capacity,
                                                         requested: bytes })?;
            *counters.counter(charge) += bytes;
            counters.admitted = next;
            debug_assert_eq!(counters.admitted,
                             counters.foundation
                             + counters.system
                             + counters.domains
                             + counters.raw_backing,
                             "protected recovery categories must sum to admitted backing");
            Ok(())
        })
    }

    fn refund(&self, charge: Charge, bytes: u64)
    {
        let valid = with_masked(|| {
            let mut counters = self.counters.lock();
            let Some(admitted) = counters.admitted.checked_sub(bytes)
            else
            {
                return false;
            };
            let Some(category) = counters.counter(charge).checked_sub(bytes)
            else
            {
                return false;
            };
            counters.admitted = admitted;
            *counters.counter(charge) = category;
            debug_assert_eq!(counters.admitted,
                             counters.foundation
                             + counters.system
                             + counters.domains
                             + counters.raw_backing,
                             "protected recovery refunds must preserve category totals");
            true
        });
        if !valid
        {
            crate::machine::emergency::enter(crate::emergency::Kind::SystemPanic,
                                             format_args!("Protected recovery allocation refund \
                                                           exceeds its original admission"));
        }
    }

    fn allocate(&self, layout: Layout) -> Result<NonNull<u8>, Error>
    {
        with_masked(|| {
            let mut heap = self.heap.lock();
            heap.allocate_first_fit(layout).map_err(|_| {
                                               Error::BackingRefused { requested: layout.size(),
                                                                       alignment: layout.align(),
                                                                       free:      heap.free() }
                                           })
        })
    }

    /// # Safety
    /// Exact live allocation and true backend layout from this storage owner.
    unsafe fn deallocate(&self, pointer: NonNull<u8>, layout: Layout)
    {
        with_masked(|| unsafe { self.heap.lock().deallocate(pointer, layout) });
    }
}
impl Permit
{
    pub(super) fn reserve(&self, category: budget::Category, bytes: u64) -> Result<Receipt, Error>
    {
        self.storage.reserve(Charge::Allocation(category), bytes)?;
        Ok(Receipt { storage: self.storage,
                     category,
                     bytes })
    }

    /// Unique zeroed physical backing, without a GlobalAlloc header. The caller
    /// retains the genuine canonical stack/CPU charge outside this lease. Raw
    /// storage occupancy is counted once here, separately from ordinary memory.
    pub(crate) fn allocate_backing(&self, requested: Layout) -> Result<Backing, Error>
    {
        if requested.size() == 0
        {
            return Err(Error::Layout);
        }
        let layout = HoleList::align_layout(requested).map_err(|_| Error::Layout)?;
        let bytes = layout.size() as u64;
        self.storage.reserve(Charge::RawBacking, bytes)?;
        let receipt = RawReceipt { storage: self.storage,
                                   bytes };
        let base = self.storage.allocate(layout)?;
        let physical =
            base.as_ptr()
                .addr()
                .checked_sub(self.storage.range.cpu_base())
                .filter(|&offset| {
                    offset.checked_add(layout.size())
                          .is_some_and(|end| end as u64 <= self.storage.range.bytes())
                })
                .and_then(|offset| self.storage.range.physical().0.checked_add(offset as u64));
        let Some(physical) = physical
        else
        {
            // Reject impossible backend geometry without losing the unique
            // allocation or refund; no resource/heap guard crosses disposal.
            unsafe { self.storage.deallocate(base, layout) };
            drop(receipt);
            return Err(Error::Layout);
        };
        // SAFETY: this unpublished allocation is exclusive. Bulk initialization
        // happens after native free-list/IRQ guards have been released.
        unsafe { ptr::write_bytes(base.as_ptr(), 0, layout.size()) };
        Ok(Backing { base,
                     layout,
                     physical: PhysicalAddress(physical),
                     receipt })
    }
}

/// Header-owned protected origin; it preserves category, storage and full true
/// layout charge across realloc/free without consulting the current caller.
pub(super) struct Receipt
{
    storage:  &'static Storage,
    category: budget::Category,
    bytes:    u64
}
impl Receipt
{
    pub(super) fn reserve_replacement(&self, bytes: u64) -> Result<Self, Error>
    {
        Permit { storage: self.storage }.reserve(self.category, bytes)
    }

    pub(super) fn allocate(&self, layout: Layout) -> Result<NonNull<u8>, Error>
    {
        self.storage.allocate(layout)
    }

    pub(super) unsafe fn deallocate(&self, pointer: NonNull<u8>, layout: Layout)
    {
        unsafe { self.storage.deallocate(pointer, layout) };
    }
}
impl Drop for Receipt
{
    fn drop(&mut self)
    {
        self.storage.refund(Charge::Allocation(self.category), self.bytes);
    }
}
struct RawReceipt
{
    storage: &'static Storage,
    bytes:   u64
}
impl Drop for RawReceipt
{
    fn drop(&mut self)
    {
        self.storage.refund(Charge::RawBacking, self.bytes);
    }
}
/// Owned protected raw backing. No clone, reference rebasing, second buddy
/// token or ordinary fallback exists. Return backing before its occupancy refund.
pub(crate) struct Backing
{
    base:     NonNull<u8>,
    layout:   Layout,
    physical: PhysicalAddress,
    receipt:  RawReceipt
}
// SAFETY: the unique allocation may move between threads; no typed view or
// borrowed pointer is lent here. Its reset-life storage synchronizes its free list.
unsafe impl Send for Backing
{
}
// SAFETY: shared access exposes only immutable address/extent metadata, never
// a reference or slice into the allocation. Scrubbing requires &mut Backing,
// and disposal consumes the unique lease. Dereferencing an address (including
// a live CPU stack) remains the architecture owner's audited unsafe boundary;
// Sync here grants no concurrent access to those bytes. Storage serializes its
// own free list and occupancy accounting independently of this lease.
unsafe impl Sync for Backing
{
}
impl Backing
{
    pub(crate) fn cpu_base(&self) -> usize
    {
        self.base.as_ptr().addr()
    }

    pub(crate) fn physical(&self) -> PhysicalAddress
    {
        self.physical
    }

    pub(crate) fn bytes(&self) -> u64
    {
        self.layout.size() as u64
    }

    /// Scrub a checked portion of exclusively owned, quiescent raw backing.
    /// No allocator lock or additional interrupt exclusion spans the byte work.
    pub(crate) fn zero_range(&mut self, offset: usize, bytes: usize) -> Result<(), Error>
    {
        offset.checked_add(bytes).filter(|&end| end <= self.layout.size()).ok_or(Error::Layout)?;
        // SAFETY: checked subrange of this exclusive untyped backing lease.
        unsafe { ptr::write_bytes(self.base.as_ptr().add(offset), 0, bytes) };
        Ok(())
    }
}
impl Drop for Backing
{
    fn drop(&mut self)
    {
        // SAFETY: this unique lease retains its exact owner and backend layout.
        // Its receipt field refunds only after Drop returns and the guard is gone.
        unsafe { self.receipt.storage.deallocate(self.base, self.layout) };
    }
}
