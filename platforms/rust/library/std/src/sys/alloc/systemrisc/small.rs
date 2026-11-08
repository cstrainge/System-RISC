//! Process-local size classes over System-owned backing allocations.
//! The owner account charges the entire backing allocation while it is mapped.
use super::{backing_alloc, backing_free};
use crate::{alloc::Layout, cell::UnsafeCell, ptr, sys::sync::Mutex};

const ALIGN: usize = 16;
const MIN_SHIFT: u32 = 5;
const CLASSES: usize = 10;
const MIN_BACKING: usize = 16 * 1024;
// Refill policy, not an allocation limit. Budget refusal falls back as far as
// one slot, and unused arenas remain eligible for trim before returning OOM.
const MAX_BACKING: usize = 256 * 1024;

pub(super) fn class(layout: Layout) -> Option<usize>
{
    let maximum = (1 << (MIN_SHIFT + CLASSES as u32 - 1)) - ALIGN;
    if layout.align() > ALIGN || layout.size() > maximum
    {
        return None;
    }
    let stride = (layout.size() + ALIGN).max(1 << MIN_SHIFT).next_power_of_two();
    Some((stride.trailing_zeros() - MIN_SHIFT) as usize)
}

#[repr(align(16))]
struct Slab
{
    next:     *mut Slab,
    previous: *mut Slab,
    recycled: *mut u8,
    issued:   usize,
    live:     usize,
    capacity: usize
}

struct Lists
{
    available:   *mut Slab,
    spare:       *mut Slab,
    next_target: usize
}

impl Lists
{
    // Only partially occupied slabs are in the available list.
    unsafe fn insert(&mut self, slab: *mut Slab)
    {
        unsafe {
            (*slab).previous = ptr::null_mut();
            (*slab).next = self.available;
            if !self.available.is_null()
            {
                (*self.available).previous = slab;
            }
            self.available = slab;
        }
    }

    unsafe fn remove(&mut self, slab: *mut Slab)
    {
        unsafe {
            let previous = (*slab).previous;
            let next = (*slab).next;
            if previous.is_null()
            {
                self.available = next;
            }
            else
            {
                (*previous).next = next;
            }
            if !next.is_null()
            {
                (*next).previous = previous;
            }
        }
    }
}

struct Class
{
    lock:  Mutex,
    lists: UnsafeCell<Lists>
}

// Each list and its slab metadata are accessed under that class's mutex.
// A detached slab is exclusively owned by the caller until publication.
unsafe impl Sync for Class
{
}

impl Class
{
    const fn new() -> Self
    {
        Self { lock:  Mutex::new(),
               lists: UnsafeCell::new(Lists { available:   ptr::null_mut(),
                                              spare:       ptr::null_mut(),
                                              next_target: MIN_BACKING }) }
    }

    fn with<R>(&self, use_lists: impl FnOnce(&mut Lists) -> R) -> R
    {
        struct Unlock<'a>(&'a Mutex);
        impl Drop for Unlock<'_>
        {
            fn drop(&mut self)
            {
                unsafe { self.0.unlock() };
            }
        }
        self.lock.lock();
        let _unlock = Unlock(&self.lock);
        unsafe { use_lists(&mut *self.lists.get()) }
    }
}

static BINS: [Class; CLASSES] = [const { Class::new() }; CLASSES];

fn stride(class: usize) -> usize
{
    1 << (MIN_SHIFT + class as u32)
}

fn capacity(class: usize, target: usize) -> usize
{
    // Leave room for the backing allocator's alignment allowance, so a
    // power-of-two refill does not unnecessarily require the next buddy order.
    (target.saturating_sub(size_of::<Slab>() + ALIGN - 1) / stride(class)).max(1)
}

fn backing_layout(class: usize, capacity: usize) -> Layout
{
    let bytes = size_of::<Slab>() + capacity * stride(class);
    // The bounded class and slot counts keep bytes within Layout's limits.
    unsafe { Layout::from_size_align_unchecked(bytes, ALIGN) }
}

// The caller holds the class lock and the available slab has a free slot.
unsafe fn take(lists: &mut Lists, class: usize, slab: *mut Slab) -> *mut u8
{
    unsafe {
        let block = if (*slab).recycled.is_null()
        {
            let slot = slab.cast::<u8>().add(size_of::<Slab>() + (*slab).issued * stride(class));
            (*slab).issued += 1;
            slot.cast::<*mut Slab>().write(slab);
            slot.add(ALIGN)
        }
        else
        {
            let block = (*slab).recycled;
            (*slab).recycled = block.cast::<*mut u8>().read();
            block
        };
        (*slab).live += 1;
        if (*slab).live == (*slab).capacity
        {
            lists.remove(slab);
        }
        block
    }
}

pub(super) fn allocate(class: usize) -> *mut u8
{
    let bin = &BINS[class];
    let (block, target) = bin.with(|lists| unsafe {
                                 if lists.available.is_null() && !lists.spare.is_null()
                                 {
                                     let slab = ptr::replace(&mut lists.spare, ptr::null_mut());
                                     lists.insert(slab);
                                 }
                                 if lists.available.is_null()
                                 {
                                     let target = lists.next_target;
                                     lists.next_target = (target * 2).min(MAX_BACKING);
                                     (ptr::null_mut(), target)
                                 }
                                 else
                                 {
                                     (take(lists, class, lists.available), 0)
                                 }
                             });
    if !block.is_null()
    {
        return block;
    }

    // System may schedule or refuse growth. Hold no allocator lock across it.
    let mut slots = capacity(class, target);
    let mut used_target = target;
    let mut slab = backing_alloc(backing_layout(class, slots)).cast::<Slab>();
    if slab.is_null() && target > MIN_BACKING
    {
        used_target = MIN_BACKING;
        slots = capacity(class, used_target);
        slab = backing_alloc(backing_layout(class, slots)).cast::<Slab>();
    }
    if slab.is_null() && slots > 1
    {
        slots = 1;
        used_target = size_of::<Slab>() + stride(class) + ALIGN - 1;
        slab = backing_alloc(backing_layout(class, slots)).cast::<Slab>();
    }
    if slab.is_null()
    {
        return ptr::null_mut();
    }
    unsafe {
        slab.write(Slab { next:     ptr::null_mut(),
                          previous: ptr::null_mut(),
                          recycled: ptr::null_mut(),
                          issued:   0,
                          live:     0,
                          capacity: slots });
    }
    bin.with(|lists| unsafe {
           if used_target != target
           {
               // A smaller successful refill is evidence of current pressure.
               // Start future growth from that size instead of repeatedly
               // requesting the previously unaffordable arena.
               lists.next_target = used_target.next_power_of_two().min(MAX_BACKING);
           }
           lists.insert(slab);
           take(lists, class, slab)
       })
}

pub(super) unsafe fn release(class: usize, block: *mut u8)
{
    // GlobalAlloc guarantees a live block with the original layout.
    let slab = unsafe { block.sub(ALIGN).cast::<*mut Slab>().read() };
    let discard = BINS[class].with(|lists| unsafe {
                                 if (*slab).live == (*slab).capacity
                                 {
                                     lists.insert(slab);
                                 }
                                 block.cast::<*mut u8>().write((*slab).recycled);
                                 (*slab).recycled = block;
                                 (*slab).live -= 1;
                                 if (*slab).live != 0
                                 {
                                     return ptr::null_mut();
                                 }
                                 lists.remove(slab);
                                 // Keep one empty arena, preferring the one that
                                 // can absorb the most repeated temporary work.
                                 // trim releases it on actual budget refusal.
                                 if lists.spare.is_null()
                                    || (*slab).capacity > (*lists.spare).capacity
                                 {
                                     ptr::replace(&mut lists.spare, slab)
                                 }
                                 else
                                 {
                                     slab
                                 }
                             });
    if !discard.is_null()
    {
        unsafe { backing_free(discard.cast(), backing_layout(class, (*discard).capacity)) };
    }
}

pub(super) fn trim() -> bool
{
    let mut freed = false;
    for (class, bin) in BINS.iter().enumerate()
    {
        let slab = bin.with(|lists| unsafe { ptr::replace(&mut lists.spare, ptr::null_mut()) });
        if !slab.is_null()
        {
            unsafe { backing_free(slab.cast(), backing_layout(class, (*slab).capacity)) };
            freed = true;
        }
    }
    freed
}
