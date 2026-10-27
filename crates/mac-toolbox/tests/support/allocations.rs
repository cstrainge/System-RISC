//! Thread-local observation/refusal around real public SDK service calls.
use std::{alloc::{GlobalAlloc, Layout, System as HostAllocator},
          cell::Cell,
          thread_local};
thread_local! {
    static MODE:Cell<u8> = const { Cell::new(0) };
    static ALLOCATIONS:Cell<usize> = const { Cell::new(0) };
    static SIZE:Cell<usize> = const { Cell::new(0) };
}
struct Allocator;
fn allocation(size: usize) -> bool
{
    MODE.try_with(|mode| {
            let value = mode.get();
            let selected = value != 0
                           && (value <= 2
                               || value == 5
                               || SIZE.try_with(|target| target.get() == size).unwrap_or(false));
            let ordinal = if selected
            {
                ALLOCATIONS.try_with(|n| {
                               let next = n.get() + 1;
                               n.set(next);
                               next
                           })
                           .unwrap_or(0)
            }
            else
            {
                0
            };
            value == 2
            || value == 3 && selected
            || value == 5 && SIZE.try_with(|target| target.get() == ordinal).unwrap_or(false)
        })
        .unwrap_or(false)
}
// SAFETY: delegates layouts/pointers unchanged to the host allocator. Only the
// current test thread can select typed allocation refusal, before its API call.
unsafe impl GlobalAlloc for Allocator
{
    unsafe fn alloc(&self, layout: Layout) -> *mut u8
    {
        if allocation(layout.size())
        {
            core::ptr::null_mut()
        }
        else
        {
            unsafe { HostAllocator.alloc(layout) }
        }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout)
    {
        unsafe { HostAllocator.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8
    {
        if allocation(size)
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
    pub fn begin(refuse: bool, size: Option<usize>) -> Self
    {
        ALLOCATIONS.with(|n| n.set(0));
        SIZE.with(|target| target.set(size.unwrap_or(0)));
        MODE.with(|m| {
                assert_eq!(m.get(), 0);
                m.set(match (refuse, size.is_some())
                      {
                          (false, false) => 1,
                          (true, false) => 2,
                          (true, true) => 3,
                          (false, true) => 4
                      });
            });
        Self
    }

    /// Refuse exactly one allocation in an otherwise ordinary service call.
    /// Iterating its observed allocation ordinals exercises failures after every
    /// successfully admitted prefix, including backing hidden in dependencies.
    pub fn fail_at(ordinal: usize) -> Self
    {
        assert!(ordinal > 0);
        ALLOCATIONS.with(|n| n.set(0));
        SIZE.with(|target| target.set(ordinal));
        MODE.with(|m| {
                assert_eq!(m.get(), 0);
                m.set(5);
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
