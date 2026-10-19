//! Resident Memory Manager. Safe handle storage behind a narrow native boundary.
mod manager;
pub use manager::Manager;
pub mod native;
