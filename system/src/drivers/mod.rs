//! Native kernel modules: generic admission/lifetime plus narrow executable ABI.
mod registry;
pub use registry::*;
#[cfg(target_arch = "riscv64")]
mod runtime;
#[cfg(target_arch = "riscv64")]
pub(crate) use runtime::*;
