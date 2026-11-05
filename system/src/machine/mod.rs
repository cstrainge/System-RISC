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

#[cfg(target_arch = "riscv64")]
mod riscv64;
#[cfg(target_arch = "riscv64")]
pub(crate) use riscv64::*;

// Current hardware contract; resident lifecycle code does not choose an ISA.
// Generated from the platform record so a second board states its own target.
include!(concat!(env!("OUT_DIR"), "/platform-module-target.rs"));
