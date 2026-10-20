//! Allocation-free incremental directory enumeration.
pub use mac_abi::SCAN_STALE;
use mac_abi::files::{DirectoryScanCursor, FileScanCall, SCAN_DIRECTORY, SCAN_DONE, SCAN_ENTRY,
                     SCAN_PENDING};

use super::{CatalogInfo, Error, Files, Result};

/// Continuation bound to one volume, directory and catalog revision. Copying a
/// cursor copies its position; reset with Default to start another enumeration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirectoryCursor
{
    cursor: DirectoryScanCursor
}
/// Fixed-size returned metadata keeps the SDK usable without an allocator.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanStep
{
    Entry
    {
        catalog:            CatalogInfo,
        data_allocated:     u64,
        resource_allocated: u64
    },
    Pending,
    Done
}
impl Files<'_>
{
    /// Inspect at most eight catalog records from at most one leaf. Pending is
    /// ordinary progress: return to the event loop before advancing again.
    /// SCAN_STALE means a mutation invalidated this enumeration; discard all
    /// accumulated totals and restart with a fresh cursor.
    pub fn scan_directory(self, volume: u32, directory: u32, cursor: &mut DirectoryCursor)
                          -> Result<ScanStep>
    {
        let mut call = FileScanCall { operation: SCAN_DIRECTORY,
                                      volume,
                                      directory,
                                      cursor: cursor.cursor,
                                      ..Default::default() };
        self.client.file_scan(&mut call)?;
        let step = match call.step
        {
            SCAN_ENTRY =>
            {
                ScanStep::Entry { catalog:            call.entry,
                                  data_allocated:     call.data_allocated,
                                  resource_allocated: call.resource_allocated }
            }
            SCAN_PENDING => ScanStep::Pending,
            SCAN_DONE => ScanStep::Done,
            _ => return Err(Error::BadArgument)
        };
        cursor.cursor = call.cursor;
        Ok(step)
    }
}
