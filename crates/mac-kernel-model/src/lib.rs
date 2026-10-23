//! Architecture-neutral shapes for memory/app/resource spec §13.1.
//!
//! The current OS does not run on this model yet. No allocator, page table,
//! handle projection, process limit or trap behavior changes by linking it.
//! Page metadata/leases are safe primitives; mapping/CPU access belongs in a
//! platform adapter. Policy numbers require reviewed configuration, not defaults.
#![no_std]
#![forbid(unsafe_code)]
#![feature(allocator_ext)]
extern crate alloc;
pub use mac_object_table::domain::{DomainId, DomainKind, ResourceKey};
pub mod policy;
pub mod pages;
pub mod buddy;
pub mod process;
pub mod text;
pub mod zone;
pub mod emergency;
