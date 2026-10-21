//! Central generation-tagged object table shared by System and host providers.
//! Keep memory's original high16 generation/low16 one-based slot wire contract.
#![forbid(unsafe_code)]
pub(crate) use mac_object_table::{Kind, Table};
