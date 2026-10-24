//! Resident Memory Manager. Safe handle storage behind a narrow native boundary.
mod manager;
pub(crate) mod lifetime;
pub use manager::Manager;
pub mod native;
