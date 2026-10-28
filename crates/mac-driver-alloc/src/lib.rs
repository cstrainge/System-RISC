//! Narrow fallible heap boundary for pinned module operation state. Reserve
//! storage before moving a driver client (and its live leases) into a future;
//! OOM must leave that client available for recovery rather than dropping it.
//!
//! This is a separate crate so Cargo feature unification never imposes a heap
//! on allocation-free modules that share the core mac-driver dependency.
#![no_std]
#![feature(allocator_ext)]
extern crate alloc;
pub mod scrap;
pub mod workers;
use alloc::{alloc::{alloc, dealloc},
            boxed::Box};
use core::{alloc::Layout, ptr::NonNull};

/// Original-account resident allocator for independently loaded native modules.
pub use mac_driver::runtime::allocation::ForwardingAllocator;
use mac_driver::{Error, Result};
pub use workers::{PrepareFailure, PreparedWorker, StartFailure, Worker, WorkerContext,
                  WorkerWaker, Workers};
/// Uninitialized storage only. It cannot be read or dereferenced as T until
/// `write` consumes the reservation and transfers ownership to a normal Box.
pub struct Reservation<T>
{
    pointer: Option<NonNull<T>>
}
impl<T> Reservation<T>
{
    pub fn try_new() -> Result<Self>
    {
        let layout = Layout::new::<T>();
        let pointer = if layout.size() == 0
        {
            NonNull::dangling()
        }
        else
        {
            // SAFETY: Layout is the nonzero, correctly aligned allocation for T.
            // Ownership remains here until write() or Drop, never in the caller.
            NonNull::new(unsafe { alloc(layout) }.cast::<T>()).ok_or(Error(mac_abi::NO_MEMORY))?
        };
        Ok(Self { pointer: Some(pointer) })
    }

    pub fn write(mut self, value: T) -> Box<T>
    {
        let pointer = self.pointer.take().expect("reservation owns its allocation").as_ptr();
        // SAFETY: This exclusive allocation has enough space/alignment for T,
        // including the standard dangling pointer for a ZST. It is initialized
        // once; removing pointer prevents this wrapper from deallocating it.
        // Box then owns both T's destructor and the matching global allocation.
        unsafe {
            pointer.write(value);
            Box::from_raw(pointer)
        }
    }
}
impl<T> Drop for Reservation<T>
{
    fn drop(&mut self)
    {
        if let Some(pointer) = self.pointer
           && size_of::<T>() != 0
        {
            // SAFETY: write() did not transfer this uninitialized allocation.
            // There is no T value to drop, and the layout matches try_new().
            unsafe { dealloc(pointer.as_ptr().cast(), Layout::new::<T>()) };
        }
    }
}
