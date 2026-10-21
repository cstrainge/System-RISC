//! Machine-specific display, device and trap entry adapters.
pub(crate) mod display;
pub(crate) mod memory;
pub(crate) mod driver_devices;
pub(crate) mod input;
pub(crate) mod traps;

/// Bootstrap hardware identity only; never an application or OS-thread identity.
pub(crate) fn hart_id() -> usize
{
    let hart: usize;
    unsafe {
        core::arch::asm!("csrr {}, mhartid", out(reg) hart, options(nomem, nostack));
    }
    hart
}

pub(crate) mod clock;

pub(crate) mod scheduler;
pub(crate) mod io;
