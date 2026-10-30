//! Architecture-neutral shapes for memory/app/resource spec §13.1.
//!
//! Resident consumers adopt these primitives as their migrations land. Linking
//! this crate alone does not activate its allocator or page-table model.
//! Page metadata/leases are safe primitives; mapping/CPU access belongs in a
//! platform adapter. Policy numbers require reviewed configuration, not defaults.
#![no_std]
#![forbid(unsafe_code)]
#![feature(allocator_ext, maybe_uninit_fill)]
extern crate alloc;
pub use mac_object_table::domain::{DomainId, DomainKind, ResourceKey};
pub mod policy;
pub mod account;
pub mod pages;
pub mod stack;
pub mod buddy;
pub mod bootstrap;
pub mod process;
pub mod image;
pub mod text;
pub mod zone;
pub mod emergency;

/// Buddy-backed Application heap spans with external owned metadata.
pub mod heap;
