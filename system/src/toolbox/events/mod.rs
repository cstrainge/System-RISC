//! Resident Event Manager queue and hardware input decoding.
pub mod queue;
pub mod position;
pub mod input;
pub use mac_abi::{Point, events::*};
pub use queue::*;
