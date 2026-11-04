//! Resident Resource Manager definitions and code-resource support.
pub use mac_abi::ResourceKey;
pub use mac_formats::{code, resource};
pub(crate) mod definitions;
#[cfg(guest)]
pub(crate) mod loader;

pub mod manager;
#[cfg(any(guest, feature = "test-support"))]
pub(crate) mod file;

#[cfg(any(guest, feature = "test-support"))]
pub(crate) mod native;

#[cfg(any(guest, feature = "test-support"))]
pub(crate) mod inspect;
