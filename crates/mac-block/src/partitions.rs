//! Partition-table readers, independent of filesystems.
//!
//! A medium states its extents in one of several table formats. Each reader is
//! kept here so the format is parsed once for every consumer, and the image
//! tool's writer has a matching reader beside it rather than each consumer
//! growing its own.

mod apple;
mod mbr;

pub use apple::{ApplePartition, apple_partitions};
pub use mbr::{MbrEntry, MbrPartition, mbr_entries, mbr_partitions};
