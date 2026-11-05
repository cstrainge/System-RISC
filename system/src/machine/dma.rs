//! Pinned, charged resident-heap DMA storage. The already admitted System heap
//! has a contiguous native bus alias; this does not round each buffer to a
//! power-of-two PageBlock or borrow arbitrary caller memory.
use alloc::alloc::{alloc, dealloc};
use core::{alloc::Layout, ptr::NonNull};

use mac_abi::{BAD_ARGUMENT, NO_MEMORY, NOT_SUPPORTED, Status};

pub(crate) struct Buffer
{
    pointer:     NonNull<u8>,
    layout:      Layout,
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
    pub(crate) fn new(bytes: u64) -> Result<Self, Status>
    {
        let length = usize::try_from(bytes).map_err(|_| BAD_ARGUMENT)?;
        if length == 0
        {
            return Err(BAD_ARGUMENT);
        }
        let rounded = length.checked_add(4095).map(|n| n / 4096 * 4096).ok_or(BAD_ARGUMENT)?;
        let layout = Layout::from_size_align(rounded, 4096).map_err(|_| BAD_ARGUMENT)?;
        // The authenticated Device service selects the original module Kernel
        // account. GlobalAlloc retains that actual receipt in its header.
        let pointer = NonNull::new(unsafe { alloc(layout) }).ok_or(NO_MEMORY)?;
        Ok(Self { pointer,
                  layout,
                  length,
                  initialized: 0 })
    }

    pub(crate) fn advance(&mut self) -> bool
    {
        let count = (self.layout.size() - self.initialized).min(64 * 1024);
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
        self.initialized == self.layout.size()
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
        super::paging::bus_address(self.address(), self.length()).ok_or(NOT_SUPPORTED)
    }
}
impl Drop for Buffer
{
    fn drop(&mut self)
    {
        // SAFETY: same allocator/Layout, and the owning Device record can be
        // removed only after real completion/reset and CPU reader drainage.
        unsafe {
            dealloc(self.pointer.as_ptr(), self.layout);
        }
    }
}
