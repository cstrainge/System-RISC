//! Data-cache maintenance by virtual address. The clean is the operation the
//! framebuffer path needs: the scanout reads DRAM, so a written line only
//! reaches the display after `dc cvac` writes it back to the point of
//! coherency. The invalidate exists so the experiment can prove the difference.
use core::arch::asm;

/// Smallest data-cache line, from CTR_EL0. Cortex-A53 reports 64 bytes; reading
/// it keeps the loop correct on a part that reports another size.
pub fn line_bytes() -> usize
{
    let ctr: u64;
    // SAFETY: reading a read-only identification register has no side effects.
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr, options(nostack, nomem)) };
    let log2_words = ((ctr >> 16) & 0xf) as u32;
    4usize << log2_words
}

/// Clean each line in the range to the point of coherency.
pub fn clean(start: usize, bytes: usize)
{
    // Stores still in flight when the maintenance runs would not be covered,
    // so let them reach the cache first.
    dsb();
    let line = line_bytes();
    let mut address = start & !(line - 1);
    let end = start + bytes;
    while address < end
    {
        // SAFETY: `address` is a valid address in the caller's buffer; the
        // instruction only writes that line back.
        unsafe { asm!("dc cvac, {}", in(reg) address, options(nostack, preserves_flags)) };
        address += line;
    }
    dsb();
}

/// Complete every maintenance operation issued before it.
pub fn dsb()
{
    // SAFETY: a barrier has no operands and no memory effects of its own.
    unsafe { asm!("dsb sy", options(nostack, preserves_flags)) };
}
