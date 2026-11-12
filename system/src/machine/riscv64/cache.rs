//! Data-cache maintenance. The RISC-V machines this System runs on state
//! every DMA device as sharing the CPU's view of memory, and the base ISA has
//! no cache-management instructions, so there is nothing to perform. A
//! machine that needs it has the Zicbom extension, and this is where it goes.

/// # Safety
/// None required; present so callers are the same on every architecture.
pub(crate) unsafe fn clean(_address: usize, _bytes: usize)
{
}

/// # Safety
/// None required; present so callers are the same on every architecture.
pub(crate) unsafe fn invalidate(_address: usize, _bytes: usize)
{
}
