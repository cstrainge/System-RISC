//! The machine, as the build read it from the platform record. Every address
//! and the display geometry come from here; nothing in this crate names a
//! peripheral itself.
include!(concat!(env!("OUT_DIR"), "/platform-facts.rs"));
