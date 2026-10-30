//! Build-selected minimum physical growth, separate from a process budget.
use serde::Deserialize;

use super::address;
use crate::{Result, error};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition
{
    minimum_extent_bytes: String
}
#[derive(Debug)]
pub(super) struct Checked
{
    minimum_extent_bytes: u64
}
impl Definition
{
    pub(super) fn check(&self, page: u64) -> Result<Checked>
    {
        let bytes = address("application_heap.minimum_extent_bytes", &self.minimum_extent_bytes)?;
        if !(16 * 1024..=2 * 1024 * 1024).contains(&bytes) || !bytes.is_multiple_of(page)
        {
            return Err(error("platform memory application_heap.minimum_extent_bytes must be \
                              page aligned and within the selected 16 KiB–2 MiB range"));
        }
        Ok(Checked { minimum_extent_bytes: bytes })
    }
}
impl Checked
{
    pub(super) fn constants(&self) -> [(&'static str, u64); 1]
    {
        [("APPLICATION_HEAP_MINIMUM_EXTENT_BYTES", self.minimum_extent_bytes)]
    }
}
