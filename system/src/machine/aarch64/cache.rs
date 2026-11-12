//! Data-cache maintenance, for memory a device outside the caches shares.
//!
//! The one place System performs it. A driver asks through the device
//! service; none carries these instructions itself.

fn line_bytes() -> usize
{
    let ctr: u64;
    // SAFETY: CTR_EL0 is readable at EL1 and reading it has no effect.
    unsafe { core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
    4usize << ((ctr >> 16) & 0xf)
}

/// Write cached lines covering the span out to memory.
///
/// # Safety
/// The span must be mapped in the address space this runs under.
pub(crate) unsafe fn clean(address: usize, bytes: usize)
{
    let line = line_bytes();
    let end = address.saturating_add(bytes);
    let mut at = address & !(line - 1);
    while at < end
    {
        // SAFETY: the caller states the span is mapped.
        unsafe { core::arch::asm!("dc cvac, {}", in(reg) at, options(nostack)) };
        at += line;
    }
    // SAFETY: a barrier has no memory effect.
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
}

/// Discard cached lines covering the span, so the next read asks memory.
///
/// # Safety
/// The span must be mapped, and nothing the CPU wrote to it may still be
/// wanted: a dirty line that is discarded is lost.
pub(crate) unsafe fn invalidate(address: usize, bytes: usize)
{
    let line = line_bytes();
    let end = address.saturating_add(bytes);
    let mut at = address & !(line - 1);
    while at < end
    {
        // SAFETY: the caller states the span is mapped and disposable.
        unsafe { core::arch::asm!("dc ivac, {}", in(reg) at, options(nostack)) };
        at += line;
    }
    // SAFETY: a barrier has no memory effect.
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
}
