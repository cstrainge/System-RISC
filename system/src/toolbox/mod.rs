//! Resident Toolbox implementations. Applications use the client crates.
pub mod quickdraw;
pub mod events;
pub mod scrap;
pub mod textedit;
pub mod windows;
pub mod controls;
pub mod resources;
pub mod files;
pub mod memory;
pub mod menus;
pub mod dialogs;
pub mod classic;
#[cfg(target_arch = "riscv64")]
pub(crate) mod runtime;

pub(crate) mod strings;
