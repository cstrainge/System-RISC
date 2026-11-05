//! AArch64 machine adapters.
//!
//! Only genuinely architecture-specific code belongs in this module. Logic that
//! is driven by the platform record's generated constants — physical placement,
//! DMA identity, device inventory, the scheduler's bookkeeping — is not
//! architecture-specific and must not be duplicated here; it belongs in shared
//! code above the selection point.

/// Bootstrap hardware identity only; never an application or OS-thread identity.
pub(crate) fn hart_id() -> usize
{
    cpu::hart_id()
}

pub(crate) mod clock;
pub(crate) mod cpu;
pub(crate) mod display;
pub(crate) mod interrupts;
pub(crate) mod mmu;
pub(crate) mod power;
pub(crate) mod trap_entry;
