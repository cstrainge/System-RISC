//! Resident Event Manager queue and hardware input decoding.
pub mod queue;
pub mod position;
pub mod input;
#[cfg(any(target_arch = "riscv64", test))]
pub(crate) mod priority;
pub use mac_abi::{Point, events::*};
pub use queue::*;
