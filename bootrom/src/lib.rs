//! Allocation-free bootstrap discovery; physical pointer access stays in entry code.
#![no_std]
#![feature(allocator_ext)]
#![forbid(unsafe_code)]
extern crate alloc;
pub mod completion;
pub mod devicetree;
pub mod memory;
pub mod translation;
