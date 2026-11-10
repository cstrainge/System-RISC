//! Pipe state. The byte ring is architecture-neutral and available on every
//! build so it can be exercised directly. The resident manager below is compiled
//! for every resident target, not just the one this project happens to boot most
//! often: it contains no architecture-specific code or branches, and parking a
//! writer is the scheduler's job, which both machines provide.
pub(crate) mod ring;

mod terminal;
mod manager;
pub(crate) use manager::*;
