//! Allocation-free bootstrap discovery; physical pointer access stays in entry code.
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;
pub mod completion;
pub mod devicetree;
pub mod memory;
