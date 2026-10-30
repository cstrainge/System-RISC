//! Resident Toolbox implementations. Applications use the client crates.
pub mod quickdraw;
pub mod events;
pub mod drag;
pub mod scrap;
pub mod textedit;
pub mod windows;
pub mod controls;
pub mod resources;
pub mod files;
pub(crate) mod power;
pub mod memory;
pub(crate) mod inspection;
pub(crate) mod theme;
pub(crate) mod handles;
pub mod menus;
pub mod crashes;
pub mod dialogs;
pub mod classic;
#[cfg(target_arch = "riscv64")]
pub(crate) mod runtime;

pub(crate) mod strings;

#[cfg(target_arch = "riscv64")]
pub mod processes;

pub mod standard_file;

#[cfg(all(test, feature = "test-support", not(target_arch = "riscv64")))]
#[path = "processes/menu.rs"]
pub(crate) mod test_application_menu;

#[cfg(all(test, feature = "test-support", not(target_arch = "riscv64")))]
#[path = "processes/presentation.rs"]
pub(crate) mod test_process_presentation;

#[cfg(target_arch = "riscv64")]
pub(crate) mod application_heap;
