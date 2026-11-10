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
#[cfg(systemrisc)]
pub(crate) mod entropy;
pub mod memory;
pub(crate) mod inspection;
pub(crate) mod theme;
pub(crate) mod handles;
pub mod menus;
pub mod crashes;
pub mod dialogs;
pub mod classic;
#[cfg(systemrisc)]
pub(crate) mod runtime;


#[cfg(systemrisc)]
pub mod processes;

pub(crate) mod pipes;
pub(crate) mod timers;
pub mod standard_file;


#[cfg(systemrisc)]
pub(crate) mod application_heap;

#[cfg(systemrisc)]
pub(crate) mod native_threads;
pub(crate) mod font_suitcases;
