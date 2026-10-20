//! Shared cursor vocabulary; each backend retains its own bounded scanner.
pub use mac_hfs::scan::{Cursor, ScanError};

use super::*;
// Returned by value on the scan hot path; boxing would allocate per entry.
#[allow(clippy::large_enum_variant)]
pub enum Step
{
    Entry(CatalogEntry),
    Pending,
    Done
}
pub enum CatalogScanner
{
    Hfs(mac_hfs::scan::CatalogScanner),
    HfsB(mac_hfs_b::CatalogScanner)
}
impl<D: BlockDevice> Volume<'_, D>
{
    pub fn catalog_scanner(&mut self) -> Result<CatalogScanner>
    {
        match &mut self.backend
        {
            Backend::Hfs(v) => Ok(CatalogScanner::Hfs(v.catalog_scanner()?)),
            Backend::HfsB(v) => Ok(CatalogScanner::HfsB(v.catalog_scanner()))
        }
    }
}
impl CatalogScanner
{
    pub fn step(&self, disk: &mut impl BlockDevice, parent: u32, cursor: &mut Cursor)
                -> core::result::Result<Step, ScanError>
    {
        match self
        {
            Self::Hfs(scanner) =>
            {
                Ok(match scanner.step(disk, parent, cursor)?
                {
                    mac_hfs::scan::Step::Entry(e) => Step::Entry(e.into()),
                    mac_hfs::scan::Step::Pending => Step::Pending,
                    mac_hfs::scan::Step::Done => Step::Done
                })
            }
            Self::HfsB(scanner) =>
            {
                let mut next = *cursor;
                if parent < 2
                   || next.node != 0
                   || next.previous != 0
                   || next.state > 2
                   || (next.state == 0 && next != Cursor::default())
                   || (next.state == 1 && next.index < 16)
                {
                    return Err(ScanError::InvalidCursor);
                }
                if next.state == 2
                {
                    return Ok(Step::Done);
                }
                let result = scanner.next(disk, parent, (next.state == 1).then_some(next.index))
                                    .map_err(|e| {
                                        match e
                                        {
                                            mac_hfs_b::Error::Invalid
                                            | mac_hfs_b::Error::NotFound =>
                                            {
                                                ScanError::InvalidCursor
                                            }
                                            mac_hfs_b::Error::NoMemory
                                            | mac_hfs_b::Error::Budget => ScanError::NoMemory,
                                            _ => ScanError::Catalog(native_error(e))
                                        }
                                    })?;
                let step = if let Some(e) = result
                {
                    next.index = e.id;
                    next.state = 1;
                    next.traversed =
                        next.traversed.checked_add(1).ok_or(ScanError::InvalidCursor)?;
                    Step::Entry(e.into())
                }
                else
                {
                    next.state = 2;
                    Step::Done
                };
                *cursor = next;
                Ok(step)
            }
        }
    }
}
