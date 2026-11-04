//! Machine-specific display, device and trap entry adapters.
pub(crate) mod display;
pub(crate) mod video;
pub(crate) mod memory;
pub(crate) mod pages;
pub(crate) mod driver_devices;
pub(crate) mod input;
pub(crate) mod traps;

/// Bootstrap hardware identity only; never an application or OS-thread identity.
pub(crate) fn hart_id() -> usize
{
    traps::execution::hart_id().unwrap_or(usize::MAX)
}

pub(crate) mod clock;

pub(crate) mod scheduler;
mod smp;
pub(crate) use smp::eligible_harts;
pub(crate) mod io;
pub(crate) mod interrupts;

// Current hardware contract; resident lifecycle code does not choose an ISA.
// Generated from the platform record so a second board states its own target.
include!(concat!(env!("OUT_DIR"), "/platform-module-target.rs"));

pub(crate) mod power;

pub(crate) mod paging;
pub(crate) mod monitor;
pub(crate) mod emergency;

pub(crate) mod dma;
