//! Capability-rooted Linux/host tooling for the shared native fork container.
//! The live bridge reuses raw capability I/O, not this offline catalog adapter.
#![forbid(unsafe_code)]
pub mod io;
mod volume;
mod projection;
mod import;
use std::time::{SystemTime, UNIX_EPOCH};

pub use mac_fork_file::{Fork, Kind, Metadata};
pub use volume::{Entry, Volume};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub const LOCK_RECORD: &str = ".system-risc-lock";
pub fn error(text: impl Into<String>) -> Box<dyn std::error::Error>
{
    std::io::Error::other(text.into()).into()
}
pub fn now() -> Result<u64>
{
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH)?;
    Ok(mac_hfs_b::time::Timestamp::from_unix(i64::try_from(elapsed.as_secs())?,
                                             elapsed.subsec_micros())?.0)
}
pub fn json(meta: &Metadata, layout: mac_fork_file::Layout) -> serde_json::Value
{
    serde_json::json!({"format":"system-risc-forks","version":1,"id":meta.catalog_id,"name":meta.name,"directory":meta.kind!=Kind::File,
        "flags":meta.attributes,"created_mac_microseconds":meta.created,"modified_mac_microseconds":meta.modified,
        "backup_mac_microseconds":meta.backup,"finder_info":meta.finder_info.to_vec(),"comment":meta.comment,
        "data_length":layout.data.length,"resource_length":layout.resources.length})
}
