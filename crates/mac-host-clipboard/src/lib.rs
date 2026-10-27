//! Pure guest clipboard policy for QEMU's standard SPICE vdagent channel.
//! Epochs and content versions are protocol labels, never resident grants.
#![no_std]
#![forbid(unsafe_code)]

mod error;
mod output;
mod engine;

pub use engine::{Config, Content, Effect, Engine, Epoch, FetchToken, Import, ImportToken, Phase,
                 Snapshot};
pub use error::{CompletionError, Error};
pub use output::{Output, OutputToken, Sent};
