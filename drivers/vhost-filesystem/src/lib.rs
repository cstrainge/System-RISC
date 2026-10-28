//! Host fork-container filesystem. The lower transport owns host capabilities;
//! this provider owns Macintosh names, catalog metadata and both fork layouts.
//! Runtime volume/open tokens are supplied by the resident object facility.
#![no_std]
#![forbid(unsafe_code)]
#![feature(allocator_ext)]
extern crate alloc;
mod rpc;
pub mod transport;
pub mod module;
mod service;
mod volume;
pub use rpc::{Cause, Error, Reply, Result, Rpc};
pub use volume::{Create, Filesystem, MountInfo, Node, Replace};

/// Retained module storage is separate from a request's original Subject. Scope
/// only synchronous construction or capacity growth, never a poll/lower call.
fn module_allocation<T>(operation: impl FnOnce() -> Result<T>) -> Result<T>
{
    mac_driver::runtime::allocation::with_module_allocation(operation)
        .map_err(|mac_driver::Error(status)| Error::from(Cause::Transport(status)))?
}
fn module_reserve<T>(values: &mut alloc::vec::Vec<T>, additional: usize) -> Result<()>
{
    module_allocation(|| values.try_reserve(additional).map_err(|_| Cause::NoMemory.into()))
}
