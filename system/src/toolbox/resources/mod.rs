//! Resident Resource Manager definitions and code-resource support.
pub use mac_abi::ResourceKey;
pub use mac_formats::{code, resource};
pub(crate) mod definitions;
#[cfg(systemrisc)]
pub(crate) mod loader;

pub mod manager;
#[cfg(systemrisc)]
pub(crate) mod file;

#[cfg(systemrisc)]
pub(crate) mod native;

#[cfg(systemrisc)]
pub(crate) mod inspect;
