//! Maximum process key positions, prepaid by every admitted native app thread.
use serde::Deserialize;

use crate::{Result, error};
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition
{
    key_capacity: u64
}
#[derive(Debug)]
pub(super) struct Checked
{
    key_capacity: u64
}
impl Definition
{
    pub(super) fn check(&self) -> Result<Checked>
    {
        if !(16..=4096).contains(&self.key_capacity)
        {
            return Err(error("platform memory tls.key_capacity must be within 16..4096"));
        }
        Ok(Checked { key_capacity: self.key_capacity })
    }
}
impl Checked
{
    pub(super) fn constants(&self) -> [(&'static str, u64); 1]
    {
        [("TLS_KEY_CAPACITY", self.key_capacity)]
    }
}
