//! RISC-V machine adapters: trap entry, page tables, timer, console and devices.
pub(crate) mod display;
pub(crate) mod traps;

/// Bootstrap hardware identity only; never an application or OS-thread identity.
pub(crate) fn hart_id() -> usize
{
    traps::execution::hart_id().unwrap_or(usize::MAX)
}

pub(crate) mod clock;

pub(crate) mod scheduler;

pub(crate) mod interrupts;

pub(crate) mod power;

pub(crate) mod paging;
pub(crate) mod monitor;
pub(crate) mod emergency;

