//! Resident Resource Manager definitions and code-resource support.
pub use mac_abi::ResourceKey;
pub use mac_formats::{code, resource};
pub(crate) mod definitions;
#[cfg(target_arch = "riscv64")]
pub(crate) mod loader;

pub mod manager;

#[cfg(any(target_arch = "riscv64", feature = "test-support"))]
pub(crate) mod native;
