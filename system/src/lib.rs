//! Resident System RISC code. This crate is not an application dependency.
#![no_std]
#![recursion_limit = "256"]
#![feature(allocator_ext)]
extern crate alloc;
#[cfg(systemrisc)]
macro_rules! serial { ($($arg:tt)*) => {{ use core::fmt::Write as _; let _ = writeln!($crate::kernel::Serial, $($arg)*); }} }
pub(crate) mod display;
pub(crate) mod emergency;
pub mod toolbox;
pub mod drivers;
pub(crate) mod state;
pub(crate) mod context;
pub(crate) mod allocation;
#[cfg(systemrisc)]
pub mod kernel;
#[cfg(systemrisc)]
pub(crate) mod images;
#[cfg(systemrisc)]
mod allocator;
#[cfg(systemrisc)]
pub(crate) mod recovery;
#[cfg(systemrisc)]
mod machine;
#[cfg(systemrisc)]
mod trace;
#[cfg(systemrisc)]
use machine::{display as platform, input as virtio_input};
#[cfg(systemrisc)]
use toolbox::resources::loader;

#[cfg(systemrisc)]
mod boot_screen;
