//! Resident System RISC code. This crate is not an application dependency.
#![no_std]
#![feature(allocator_ext)]
extern crate alloc;
#[cfg(target_arch = "riscv64")]
macro_rules! serial { ($($arg:tt)*) => {{ use core::fmt::Write as _; let _ = writeln!($crate::kernel::Serial, $($arg)*); }} }
pub mod toolbox;
pub mod drivers;
pub(crate) mod state;
pub(crate) mod context;
#[cfg(target_arch = "riscv64")]
pub mod kernel;
#[cfg(target_arch = "riscv64")]
mod allocator;
#[cfg(target_arch = "riscv64")]
mod machine;
#[cfg(target_arch = "riscv64")]
mod trace;
#[cfg(target_arch = "riscv64")]
use machine::{display as platform, input as virtio_input};
#[cfg(target_arch = "riscv64")]
use toolbox::resources::loader;

#[cfg(test)]
#[path = "../../definitions/wdef/src/main.rs"]
pub(crate) mod test_wdef;
#[cfg(any(test, feature = "test-support"))]
#[path = "../../definitions/buttons/src/main.rs"]
pub(crate) mod test_cdef;
#[cfg(any(test, feature = "test-support"))]
#[path = "../../definitions/scrollbar/src/main.rs"]
pub(crate) mod test_scrollbar;

#[cfg(any(test, feature = "test-support"))]
#[path = "../../definitions/progress-bar/src/main.rs"]
pub(crate) mod test_progress_bar;
#[cfg(any(test, feature = "test-support"))]
#[path = "../../definitions/static-text/src/main.rs"]
pub(crate) mod test_static_text;

#[cfg(all(feature = "test-support", not(target_arch = "riscv64")))]
#[path = "../../definitions/mdef/src/main.rs"]
pub(crate) mod test_mdef;

#[cfg(all(feature = "test-support", not(target_arch = "riscv64")))]
extern crate std;
#[cfg(all(feature = "test-support", not(target_arch = "riscv64")))]
pub mod testing;

#[cfg(target_arch = "riscv64")]
mod boot_screen;
