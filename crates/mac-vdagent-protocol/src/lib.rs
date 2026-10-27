//! Standard SPICE vdagent clipboard wire contract for the QEMU guest channel.
//! Wire labels are not guest object or registration authority. The bridge owns
//! connection epochs, grants, in-flight requests and Scrap revision validation.
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod wire;
mod decoder;
mod outbound;

pub use decoder::{Decoder, Feed, FeedError, IgnoredChunk};
pub use outbound::{Chunk, Chunks, OwnedMessage};
pub use wire::*;
