//! Pipe state. The byte ring is architecture-neutral and available on every
//! build so it can be exercised directly; the resident manager below is compiled
//! only for the target, because it issues handles and parks threads. The manager
//! itself contains no architecture-specific code or branches.
pub(crate) mod ring;

#[cfg(target_arch = "riscv64")]
mod manager;
#[cfg(target_arch = "riscv64")]
pub(crate) use manager::*;
