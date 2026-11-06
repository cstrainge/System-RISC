//! The one place a driver names an instruction.
//!
//! Drivers run on more than one architecture and their ordering requirements do
//! not differ: a device must not see a descriptor before the memory it points at,
//! and a buffer shared with another agent must be at the point of coherency
//! before its address is published. The instruction that satisfies those
//! requirements differs, so it is selected here, once, and every driver calls
//! these functions without knowing which architecture it is running on.
//!
//! This is deliberately not a `#[cfg]` per call site. Four copies of the same
//! fence chain is four places to keep in step by hand, and the fifth driver to
//! need one would have copied a fifth. A driver that names an instruction has
//! taken on an architecture-specific claim it cannot check.
//!
//! A host build links these crates for the packaging tools, where no device
//! exists. There the barrier is a compiler fence: it orders the same accesses in
//! the same program, and claims nothing about hardware it is not running on.

/// Full device/CPU publication ordering.
///
/// Call this after writing a descriptor and before publishing its address to a
/// device, and after a device signals completion and before reading the memory
/// it wrote. It is ordering, not instrumentation.
#[inline]
pub fn barrier()
{
    #[cfg(target_arch = "riscv64")]
    {
        // SAFETY: a full fence has no memory operands and no side effects beyond
        // ordering.
        unsafe { core::arch::asm!("fence iorw, iorw", options(nostack)) };
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: a system-wide data synchronization barrier has no memory
        // operands and no side effects beyond ordering.
        unsafe { core::arch::asm!("dsb sy", options(nostack)) };
    }
    #[cfg(not(any(target_arch = "riscv64", target_arch = "aarch64")))]
    {
        // No device exists on a host build; this still orders the accesses the
        // caller made, which is all that can be honestly claimed here.
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    }
}

/// The architecture's cache line size in bytes.
///
/// Cache maintenance works on whole lines, so a range must be rounded outward to
/// this granularity. An architecture with no cache maintenance to perform still
/// states a line size, because callers round with it.
#[inline]
pub fn cache_line_bytes() -> usize
{
    #[cfg(target_arch = "aarch64")]
    {
        // CTR_EL0's DminLine field is log2 of the smallest data cache line,
        // counted in four-byte words: 4 << DminLine.
        let ctr: u64;
        // SAFETY: reading an identification register has no side effects.
        unsafe { core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nostack, nomem)) };
        let words = 4usize << ((ctr >> 16) & 0xf);
        return words.max(core::mem::size_of::<usize>());
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        // RISC-V's data cache, where one is present, is maintained by the fence
        // in `barrier()` for the devices this System drives, and a host build has
        // no cache of its own to describe. The value is the minimum a caller can
        // round with rather than a claim about a cache line.
        core::mem::size_of::<usize>()
    }
}

/// Clean `bytes` at `address` to the point of coherency.
///
/// Use before publishing an address to another agent that reads memory it does
/// not share a cache with: the writes must be visible before the address is.
///
/// # Safety
/// `address..address + bytes` must be a live, writable mapping owned by the
/// caller for the duration of the call.
#[inline]
pub unsafe fn clean(address: usize, bytes: usize)
{
    #[cfg(target_arch = "aarch64")]
    {
        let line = cache_line_bytes();
        let end = address.saturating_add(bytes);
        let mut at = address & !(line - 1);
        // SAFETY: the caller guarantees the range is live and writable, and each
        // address visited lies inside the range rounded outward to a line.
        unsafe {
            while at < end
            {
                core::arch::asm!("dc cvac, {}", in(reg) at, options(nostack));
                at += line;
            }
            core::arch::asm!("dsb sy", options(nostack));
        }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        // Nothing to clean to a point of coherency that `barrier()` does not
        // already order. The parameters are still consumed so a caller compiles
        // identically on every architecture.
        let _ = (address, bytes);
    }
}

/// Discard cached copies of `bytes` at `address`.
///
/// Use after another agent wrote memory this CPU may hold a stale copy of, and
/// before reading it.
///
/// # Safety
/// `address..address + bytes` must be a live, readable mapping owned by the
/// caller for the duration of the call.
#[inline]
pub unsafe fn invalidate(address: usize, bytes: usize)
{
    #[cfg(target_arch = "aarch64")]
    {
        let line = cache_line_bytes();
        let end = address.saturating_add(bytes);
        let mut at = address & !(line - 1);
        // SAFETY: the caller guarantees the range is live and readable, and each
        // address visited lies inside the range rounded outward to a line.
        unsafe {
            while at < end
            {
                core::arch::asm!("dc ivac, {}", in(reg) at, options(nostack));
                at += line;
            }
            core::arch::asm!("dsb sy", options(nostack));
        }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = (address, bytes);
    }
}
