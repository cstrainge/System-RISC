//! Data-cache maintenance by virtual address. Two agents share memory here:
//! the firmware reads the mailbox message the bootloader writes and writes
//! the reply the bootloader reads, and the scanout reads the framebuffer the
//! bootloader paints. A written line only reaches the other agent after the
//! maintenance below, so every shared range is cleaned before it is handed
//! over and invalidated after the other side has written it.
use core::arch::asm;

/// Smallest data-cache line, from CTR_EL0. Cortex-A53 reports 64 bytes;
/// reading it keeps the loop correct on a part that reports another size.
pub fn line_bytes() -> usize
{
    let ctr: u64;
    // SAFETY: reading a read-only identification register has no side effects.
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr, options(nostack, nomem)) };
    let log2_words = ((ctr >> 16) & 0xf) as u32;
    4usize << log2_words
}

fn dsb()
{
    // SAFETY: a barrier has no memory effect beyond ordering.
    unsafe { asm!("dsb sy", options(nostack, preserves_flags)) };
}

/// Clean each line in the range to the point of coherency, so the other agent
/// reads what was written rather than a stale line.
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

/// Discard each line so the next read comes from memory. On Cortex-A53 a dirty
/// line is written back before it is invalidated, so this is safe for a
/// destination the other agent has just written, but it is not a way to
/// observe a dirty line's absence.
pub fn invalidate(start: usize, bytes: usize)
{
    dsb();
    let line = line_bytes();
    let mut address = start & !(line - 1);
    let end = start + bytes;
    while address < end
    {
        // SAFETY: `address` is a valid address in the caller's buffer; the
        // instruction discards that line after any required writeback.
        unsafe { asm!("dc ivac, {}", in(reg) address, options(nostack, preserves_flags)) };
        address += line;
    }
    dsb();
}
