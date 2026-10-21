//! Host fork-container filesystem. The lower transport owns host capabilities;
//! this provider owns Macintosh names, catalog metadata and both fork layouts.
//! Runtime volume/open tokens are supplied by the resident object facility.
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;
mod rpc;
pub mod transport;
mod service;
mod volume;
pub use rpc::{Cause, Error, Reply, Result, Rpc};
pub use volume::{Create, Filesystem, MountInfo, Node, Replace};
