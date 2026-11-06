//! Pipe state. The byte ring is architecture-neutral and available on every
//! build so it can be exercised directly; the resident manager below issues
//! handles and parks threads through the shared scheduler, and contains no
//! architecture-specific code or branches.
//!
//! The manager was gated on RISC-V only because the scheduler it parks through
//! was filed under an architecture. Now that the scheduler is shared, the gate
//! is gone: it was a consequence of the old layout, not a property of the code.
pub(crate) mod ring;
pub(crate) mod manager;
pub(crate) use manager::*;
