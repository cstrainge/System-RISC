//! Machine adapters, selected once by the target the platform record declares.
//!
//! Everything above this module consumes the selected implementation and never
//! names an architecture. A new board is a new implementation here plus a
//! record, not a conditional in shared code: adding `if target_arch` anywhere
//! above this point would mean the boundary has leaked.
//!
//! Each implementation provides the same surface: trap entry and system calls,
//! page tables and address-space activation, the timer and clock, the console,
//! interrupt control, CPU identity and secondary startup, and the platform's
//! declared devices.

// Machine-neutral adapters: they use only the generated platform constants and
// the selected implementation, so they are written once for every architecture.
pub(crate) mod display;
pub(crate) mod dma;
pub(crate) mod driver_devices;
pub(crate) mod emergency;
pub(crate) mod input;
pub(crate) mod io;
pub(crate) mod memory;
pub(crate) mod pages;
pub(crate) mod platform_devices;
pub(crate) mod power_shutdown;
pub(crate) mod smp;
pub(crate) use smp::eligible_harts;
pub(crate) mod input_class;
pub(crate) mod video;

// The scheduler, trap dispatch, pager and monitor are one implementation for
// every machine. They live here rather than inside an architecture module
// because they contain no ISA mechanism: what they need from the hardware is
// reached through the selected adapter, and the parts that do touch it — trap
// entry, interrupt masking, the timer and the wait instruction — are in that
// adapter's own `trap_entry`, `interrupts`, `cpu` and `power` modules. That is
// what makes a second architecture an adapter rather than a copy of these trees.
pub(crate) mod monitor;
pub(crate) mod paging;
pub(crate) mod scheduler;
pub(crate) mod service_failures;
pub(crate) mod traps;

#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "riscv64")]
pub(crate) use riscv64::*;
#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub(crate) use aarch64::*;

// Current hardware contract; resident lifecycle code does not choose an ISA.
// Generated from the platform record so a second board states its own target.
include!(concat!(env!("OUT_DIR"), "/platform-module-target.rs"));
