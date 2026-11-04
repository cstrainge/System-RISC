//! Resident Event Manager queue and hardware input decoding.
pub mod queue;
pub mod position;
pub mod input;
#[cfg(any(systemrisc, test))]
pub(crate) mod priority;
pub use mac_abi::{geometry::Point, events::*};
pub use queue::*;
