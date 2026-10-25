//! Checked build-platform geometry shared by resident services and host conformance.
#![forbid(unsafe_code)]
include!(concat!(env!("OUT_DIR"), "/platform-display.rs"));
pub(crate) const PIXELS: usize = WIDTH * HEIGHT;
