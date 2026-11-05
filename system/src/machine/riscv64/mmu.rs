//! Address-space activation and user-memory access.
//!
//! Selecting a translation root and flushing stale translations are CPU
//! instructions with a different register and a different flush on each
//! architecture; everything above this builds address spaces without knowing
//! which register holds the root.
use core::arch::asm;

/// The current root register value, as last written.
pub(crate) fn root() -> usize
{
    let value;
    // SAFETY: reading satp has no side effects.
    unsafe { asm!("csrr {}, satp", out(reg) value, options(nostack)) };
    value
}

/// Selects a translation root without flushing. Only used where the caller has
/// already established that no stale translation can be observed.
///
/// # Safety
/// The caller must have proved that `value` names a root whose tables are live
/// and that no in-flight access can observe a stale translation.
pub(crate) unsafe fn write_root(value: usize)
{
    // SAFETY: the caller guarantees the root is live.
    unsafe { asm!("csrw satp, {}", in(reg) value, options(nostack)) };
}

/// Discards every cached translation for the current address space.
pub(crate) fn flush_translations()
{
    // SAFETY: a full flush cannot fault and needs no operands.
    unsafe { asm!("sfence.vma zero, zero", options(nostack)) };
}

/// Selects a translation root and discards cached translations atomically with
/// respect to the caller's proof that nothing else is running.
///
/// # Safety
/// As `write_root`; additionally no other context may depend on the previous
/// root after this call.
pub(crate) unsafe fn activate(value: usize)
{
    // SAFETY: the caller guarantees the root is live.
    unsafe { asm!("csrw satp, {}", "sfence.vma zero, zero", in(reg) value, options(nostack)) };
}

/// The supervisor status register, whose bits report the active privilege and
/// whether supervisor interrupts are pending.
pub(crate) fn status() -> usize
{
    let value;
    // SAFETY: reading sstatus has no side effects.
    unsafe { asm!("csrr {}, sstatus", out(reg) value, options(nostack)) };
    value
}

/// Reads one machine word from an address the caller has already validated.
///
/// # Safety
/// `address` must be readable and correctly aligned for a word.
pub(crate) unsafe fn read_word(address: usize) -> usize
{
    let value;
    // SAFETY: the caller guarantees the address and alignment.
    unsafe {
        asm!("ld {}, 0({})", out(reg) value, in(reg) address, options(nostack, readonly));
    }
    value
}

/// Writes one machine word to an address the caller has already validated.
///
/// # Safety
/// `address` must be writable and correctly aligned for a word.
pub(crate) unsafe fn write_word(address: usize, value: usize)
{
    // SAFETY: the caller guarantees the address and alignment.
    unsafe { asm!("sd {}, 0({})", in(reg) value, in(reg) address, options(nostack)) };
}

/// Reads one byte from an address the caller has already validated.
///
/// # Safety
/// `address` must be readable.
pub(crate) unsafe fn read_byte(address: usize) -> u8
{
    let value: u32;
    // SAFETY: the caller guarantees the address.
    unsafe {
        asm!("lbu {}, 0({})", out(reg) value, in(reg) address, options(nostack, readonly));
    }
    value as u8
}

/// Writes one byte to an address the caller has already validated.
///
/// # Safety
/// `address` must be writable.
pub(crate) unsafe fn write_byte(address: usize, value: u8)
{
    // SAFETY: the caller guarantees the address.
    unsafe {
        asm!("sb {}, 0({})", in(reg) u32::from(value), in(reg) address, options(nostack));
    }
}

/// Clears one machine word at an address the caller has already validated.
///
/// # Safety
/// `address` must be writable and correctly aligned for a word.
pub(crate) unsafe fn zero_word(address: usize)
{
    // SAFETY: the caller guarantees the address and alignment.
    unsafe { asm!("sd zero, 0({})", in(reg) address, options(nostack)) };
}

/// Clears one byte at an address the caller has already validated.
///
/// # Safety
/// `address` must be writable.
pub(crate) unsafe fn zero_byte(address: usize)
{
    // SAFETY: the caller guarantees the address.
    unsafe { asm!("sb zero, 0({})", in(reg) address, options(nostack)) };
}
