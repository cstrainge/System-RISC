//! RISC-V machine adapters: trap entry, page tables, timer, console and devices.

pub(crate) mod boot_stack;
pub(crate) mod clock;
pub(crate) mod cpu;
pub(crate) mod display;
pub(crate) mod interrupts;
pub(crate) mod mmu;
pub(crate) mod page_layout;
pub(crate) mod power;
pub(crate) mod primary_service;
pub(crate) mod trap_entry;

/// Bootstrap hardware identity only; never an application or OS-thread identity.
pub(crate) fn hart_id() -> usize
{
    crate::machine::traps::execution::hart_id().unwrap_or(usize::MAX)
}
