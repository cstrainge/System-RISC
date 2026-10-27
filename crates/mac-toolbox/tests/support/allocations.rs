//! Thread-local observation/refusal around real public SDK service calls.
use std::{alloc::{GlobalAlloc, Layout, System as HostAllocator},
          cell::Cell};
thread_local! {
    static MODE:Cell<u8> = const { Cell::new(0) };
    static ALLOCATIONS:Cell<usize> = const { Cell::new(0) };
}
struct Allocator;
fn allocation() -> bool
{
    MODE.try_with(|mode| {
            let value = mode.get();
            if value != 0
            {
                let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
            }
            value == 2
        })
        .unwrap_or(false)
}
// SAFETY: delegates layouts/pointers unchanged to the host allocator. Only the
// current test thread can select typed allocation refusal, before its API call.
unsafe impl GlobalAlloc for Allocator
{
    unsafe fn alloc(&self, layout: Layout) -> *mut u8
    {
        if allocation() { core::ptr::null_mut() } else { unsafe { HostAllocator.alloc(layout) } }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout)
    {
        unsafe { HostAllocator.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8
    {
        if allocation()
        {
            core::ptr::null_mut()
        }
        else
        {
            unsafe { HostAllocator.realloc(pointer, layout, size) }
        }
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;
pub struct Observe;
impl Observe
{
    pub fn begin(refuse: bool) -> Self
    {
        ALLOCATIONS.with(|n| n.set(0));
        MODE.with(|m| {
                assert_eq!(m.get(), 0);
                m.set(if refuse { 2 } else { 1 });
            });
        Self
    }

    pub fn finish(self) -> usize
    {
        drop(self);
        ALLOCATIONS.with(Cell::get)
    }
}
impl Drop for Observe
{
    fn drop(&mut self)
    {
        MODE.with(|m| m.set(0));
    }
}
