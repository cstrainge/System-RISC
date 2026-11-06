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
    // SAFETY: reading TTBR0_EL1 has no side effects.
    unsafe { asm!("mrs {}, ttbr0_el1", out(reg) value) };
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
    unsafe {
        asm!("msr ttbr0_el1, {}", "isb", in(reg) value);
    }
}

/// Discards every cached translation for the current address space.
pub(crate) fn flush_translations()
{
    // SAFETY: a full TLB invalidate cannot fault and needs no operands.
    unsafe {
        asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb", options(nostack));
    }
}

/// Selects a translation root and discards cached translations atomically with
/// respect to the caller's proof that nothing else is running.
///
/// # Safety
/// As `write_root`; additionally no other context may depend on the previous
/// root after this call.
pub(crate) unsafe fn activate(value: usize)
{
    // SAFETY: the caller guarantees the root is live. The barrier order matters:
    // the new root must be visible before the invalidate retires, or a stale
    // entry can survive into the new space.
    unsafe {
        asm!("dsb ishst", "msr ttbr0_el1, {}", "isb", "tlbi vmalle1is", "dsb ish", "isb",
             in(reg) value, options(nostack));
    }
}

/// The saved program status, whose bits report the active exception level and
/// whether interrupts are masked.
pub(crate) fn status() -> usize
{
    let value;
    // SAFETY: reading DAIF has no side effects.
    //
    // DAIF, not SPSR_EL1. SPSR_EL1 only holds a meaningful status during an
    // exception return; outside one it is whatever was last written, which here
    // is zero. Every caller asks whether interrupts are currently masked, and
    // that is PSTATE.DAIF - bit 7 is the IRQ mask.
    unsafe { asm!("mrs {}, daif", out(reg) value) };
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
    unsafe { asm!("ldr {}, [{}]", out(reg) value, in(reg) address, options(nostack, readonly)) };
    value
}

/// Writes one machine word to an address the caller has already validated.
///
/// # Safety
/// `address` must be writable and correctly aligned for a word.
pub(crate) unsafe fn write_word(address: usize, value: usize)
{
    // SAFETY: the caller guarantees the address and alignment.
    unsafe { asm!("str {}, [{}]", in(reg) value, in(reg) address, options(nostack)) };
}

/// Reads one byte from an address the caller has already validated.
///
/// # Safety
/// `address` must be readable.
pub(crate) unsafe fn read_byte(address: usize) -> u8
{
    let value: u32;
    // SAFETY: the caller guarantees the address. The `:w` modifier selects the
    // 32-bit view of the register, which is what a byte load writes into.
    unsafe {
        asm!("ldrb {value:w}, [{address}]", value = out(reg) value, address = in(reg) address,
             options(nostack, readonly));
    }
    value as u8
}

/// Writes one byte to an address the caller has already validated.
///
/// # Safety
/// `address` must be writable.
pub(crate) unsafe fn write_byte(address: usize, value: u8)
{
    // SAFETY: the caller guarantees the address. A byte store reads the 32-bit
    // view of the value register.
    unsafe {
        asm!("strb {value:w}, [{address}]", value = in(reg) u32::from(value),
             address = in(reg) address, options(nostack));
    }
}

/// Clears one machine word at an address the caller has already validated.
///
/// # Safety
/// `address` must be writable and correctly aligned for a word.
pub(crate) unsafe fn zero_word(address: usize)
{
    // SAFETY: the caller guarantees the address and alignment.
    unsafe {
        asm!("str {zero}, [{address}]", zero = in(reg) 0usize, address = in(reg) address,
             options(nostack));
    }
}

/// Clears one byte at an address the caller has already validated.
///
/// # Safety
/// `address` must be writable.
pub(crate) unsafe fn zero_byte(address: usize)
{
    // SAFETY: the caller guarantees the address.
    unsafe {
        asm!("strb {zero:w}, [{address}]", zero = in(reg) 0u32, address = in(reg) address,
             options(nostack));
    }
}
