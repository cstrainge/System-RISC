//! Pinned DMA storage, of two kinds.
//!
//! Ordinary storage is charged resident heap: the already admitted System heap
//! has a contiguous native bus alias, and this does not round each buffer to a
//! power-of-two PageBlock or borrow arbitrary caller memory. It is cached, so
//! it suits a device that shares the CPU's caches, or a buffer no device reads.
//!
//! Uncached storage comes from the pool the platform record declares, which
//! both roots map without caching and map once. A buffer from it needs no
//! cache maintenance by anyone: there is no cached copy to go stale. It is for
//! what a device outside the CPU's caches reads and writes directly, and it is
//! small, because uncached memory is slow and such a device's control
//! structures are small. Bulk data stays in cached memory and is synchronized
//! explicitly instead.
use alloc::alloc::{alloc, dealloc};
use core::{alloc::Layout, ptr::NonNull};

use mac_abi::{BAD_ARGUMENT, NO_MEMORY, NOT_SUPPORTED, Status};

const PAGE: usize = 4096;
/// The largest pool the page map below describes: 16 MiB.
const POOL_PAGES: usize = 4096;

/// Which pool pages are held. A page map rather than a free list because the
/// pool is a few hundred pages and nothing may be written into uncached device
/// memory to track it.
struct Pool
{
    held: [u64; POOL_PAGES / 64]
}
static POOL: spinning_top::Spinlock<Pool> =
    spinning_top::Spinlock::new(Pool { held: [0; POOL_PAGES / 64] });

impl Pool
{
    fn is_held(&self, page: usize) -> bool
    {
        self.held[page / 64] & (1 << (page % 64)) != 0
    }

    fn set(&mut self, first: usize, count: usize, held: bool)
    {
        for page in first..first + count
        {
            if held
            {
                self.held[page / 64] |= 1 << (page % 64);
            }
            else
            {
                self.held[page / 64] &= !(1 << (page % 64));
            }
        }
    }

    /// First fit. The pool is small and allocation is rare, so the scan is
    /// the whole cost and there is no fragmentation policy to get wrong.
    fn take(&mut self, total: usize, count: usize) -> Option<usize>
    {
        let mut run = 0;
        for page in 0..total.min(POOL_PAGES)
        {
            run = if self.is_held(page) { 0 } else { run + 1 };
            if run == count
            {
                let first = page + 1 - count;
                self.set(first, count, true);
                return Some(first);
            }
        }
        None
    }
}

enum Storage
{
    Heap(Layout),
    Uncached
    {
        first: usize,
        pages: usize
    }
}

pub(crate) struct Buffer
{
    pointer:     NonNull<u8>,
    storage:     Storage,
    rounded:     usize,
    length:      usize,
    initialized: usize
}
// SAFETY: no Rust data references escape. Initialization requires exclusive
// access before publication. Afterwards only raw coherent DMA/renderer access
// is permitted; the Device Manager retains storage until GPU completion/reset
// and all CPU view pins have drained.
unsafe impl Send for Buffer
{
}
unsafe impl Sync for Buffer
{
}
impl Buffer
{
    fn rounded(bytes: u64) -> Result<(usize, usize), Status>
    {
        let length = usize::try_from(bytes).map_err(|_| BAD_ARGUMENT)?;
        if length == 0
        {
            return Err(BAD_ARGUMENT);
        }
        let rounded = length.checked_add(PAGE - 1).map(|n| n / PAGE * PAGE).ok_or(BAD_ARGUMENT)?;
        Ok((length, rounded))
    }

    pub(crate) fn new(bytes: u64) -> Result<Self, Status>
    {
        let (length, rounded) = Self::rounded(bytes)?;
        let layout = Layout::from_size_align(rounded, PAGE).map_err(|_| BAD_ARGUMENT)?;
        // The authenticated Device service selects the original module Kernel
        // account. GlobalAlloc retains that actual receipt in its header.
        let pointer = NonNull::new(unsafe { alloc(layout) }).ok_or(NO_MEMORY)?;
        Ok(Self { pointer,
                  storage: Storage::Heap(layout),
                  rounded,
                  length,
                  initialized: 0 })
    }

    /// Storage no cache holds a copy of. A machine that declares no pool has
    /// no such storage, and says so rather than handing back cached memory a
    /// device would read stale; a pool with no run long enough is out of
    /// memory, which is a different answer.
    pub(crate) fn new_uncached(bytes: u64) -> Result<Self, Status>
    {
        let (length, rounded) = Self::rounded(bytes)?;
        let (base, pool_bytes) = super::paging::dma_pool().ok_or(NOT_SUPPORTED)?;
        let pages = rounded / PAGE;
        let first = POOL.lock().take(pool_bytes / PAGE, pages).ok_or(NO_MEMORY)?;
        let pointer = NonNull::new((base + first * PAGE) as *mut u8).ok_or(NO_MEMORY)?;
        Ok(Self { pointer,
                  storage: Storage::Uncached { first,
                                               pages },
                  rounded,
                  length,
                  initialized: 0 })
    }

    pub(crate) fn uncached(&self) -> bool
    {
        matches!(self.storage, Storage::Uncached { .. })
    }

    pub(crate) fn advance(&mut self) -> bool
    {
        let count = (self.rounded - self.initialized).min(64 * 1024);
        // SAFETY: unique unpublished allocation; no address has been returned
        // to a driver, device or renderer. One bounded step per real Poll.
        unsafe {
            self.pointer.as_ptr().add(self.initialized).write_bytes(0, count);
        }
        self.initialized += count;
        self.ready()
    }

    pub(crate) fn ready(&self) -> bool
    {
        self.initialized == self.rounded
    }

    pub(crate) fn address(&self) -> u64
    {
        self.pointer.as_ptr() as usize as u64
    }

    pub(crate) fn length(&self) -> u64
    {
        self.length as u64
    }

    pub(crate) fn device_address(&self) -> Result<u64, Status>
    {
        crate::machine::paging::bus_address(self.address(), self.length()).ok_or(NOT_SUPPORTED)
    }
}
impl Drop for Buffer
{
    fn drop(&mut self)
    {
        match self.storage
        {
            // SAFETY: same allocator/Layout, and the owning Device record can be
            // removed only after real completion/reset and CPU reader drainage.
            Storage::Heap(layout) => unsafe {
                dealloc(self.pointer.as_ptr(), layout);
            },
            Storage::Uncached { first,
                                pages } => POOL.lock().set(first, pages, false)
        }
    }
}
