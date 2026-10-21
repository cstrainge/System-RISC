//! Native kernel modules: generic admission/lifetime plus narrow executable ABI.
pub mod devices;
pub mod file_sources;
pub mod jobs;
mod registry;
pub use registry::*;
#[cfg(target_arch = "riscv64")]
mod runtime;
#[cfg(target_arch = "riscv64")]
pub(crate) use runtime::*;
#[cfg(target_arch = "riscv64")]
pub use runtime::{OwnedJob, submit_background_job, submit_job};
