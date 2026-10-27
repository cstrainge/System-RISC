//! Current resident heap backend. All lock access goes through the architecture
//! invocation gate, including accounting reads and bootstrap initialization.
//! Page-backed budgets replace the backing later; this is the live heap's gate.
use core::alloc::{GlobalAlloc, Layout};

use linked_list_allocator::LockedHeap;

pub(crate) struct ResidentHeap
{
    heap: LockedHeap
}
impl ResidentHeap
{
    pub(crate) const fn empty() -> Self
    {
        Self { heap: LockedHeap::empty() }
    }

    /// # Safety
    /// The range must be valid exclusive backing for the allocator's lifetime,
    /// and initialization must occur exactly once, before any allocation.
    pub(crate) unsafe fn initialize(&self, start: *mut u8, length: usize)
    {
        crate::machine::interrupts::with_masked(|| {
            // SAFETY: caller supplies the uniquely owned, permanent boot range.
            unsafe {
                self.heap.lock().init(start, length);
            }
        });
    }

    pub(crate) fn statistics(&self) -> (usize, usize)
    {
        crate::machine::interrupts::with_masked(|| {
            let heap = self.heap.lock();
            (heap.used(), heap.size())
        })
    }

    pub(crate) fn available(&self) -> usize
    {
        crate::machine::interrupts::with_masked(|| self.heap.lock().free())
    }
}
// SAFETY: the backend owns its initialized range and serializes allocation and
// deallocation. Masking local preemption before its lock prevents a timer from
// switching to a caller which would spin behind this hart's suspended holder.
unsafe impl GlobalAlloc for ResidentHeap
{
    unsafe fn alloc(&self, layout: Layout) -> *mut u8
    {
        crate::machine::interrupts::with_masked(|| {
            // SAFETY: forwards the caller's GlobalAlloc layout contract.
            unsafe { self.heap.alloc(layout) }
        })
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout)
    {
        crate::machine::interrupts::with_masked(|| {
            // SAFETY: caller returns this backend's live allocation and layout.
            unsafe {
                self.heap.dealloc(pointer, layout);
            }
        });
    }
}
