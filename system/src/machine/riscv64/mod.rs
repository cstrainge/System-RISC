//! RISC-V machine adapters: trap entry, page tables, timer, console and devices.
//!
//! Only what is genuinely RISC-V belongs here. The scheduler, trap dispatch,
//! pager and monitor are shared and live one level up, in `machine`; this module
//! supplies the mechanism they call — trap entry and the frame layout, interrupt
//! masking, the timer compare and the wait instruction, the trampoline that
//! switches to a prepared stack, and the private service gate.

pub(crate) mod boot_stack;
pub(crate) mod cache;
pub(crate) mod clock;
pub(crate) mod cpu;
pub(crate) mod interrupts;
pub(crate) mod mmu;
pub(crate) mod page_layout;
pub(crate) mod power;
pub(crate) mod primary_service;
pub(crate) mod trap_entry;
pub(crate) mod trap_frame;

/// Bootstrap hardware identity only; never an application or OS-thread identity.
pub(crate) fn hart_id() -> usize
{
    super::traps::execution::hart_id().unwrap_or(usize::MAX)
}
