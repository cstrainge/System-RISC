//! Window Manager: lifecycle, z order, tracking and resource definitions.
pub(crate) mod lifetime;
mod visibility;
pub mod manager;
pub mod definition;
pub use manager::*;
