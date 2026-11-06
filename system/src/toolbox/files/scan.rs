//! One-leaf native directory scans. No remount or overflow-tree scan per step.
use core::sync::atomic::{AtomicU64, Ordering};

use mac_abi::{files::*, *};
#[cfg(any(systemrisc, feature = "test-support"))]
use mac_filesystem::scan::{Cursor, ScanError, Step};

#[cfg(any(systemrisc, feature = "test-support"))]
use super::{Manager, metadata};
static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);
pub(super) fn next_revision() -> Result<u64, Status>
{
    NEXT_REVISION.try_update(Ordering::Relaxed, Ordering::Relaxed, |revision| {
                     revision.checked_add(1)
                 })
                 .map_err(|_| NO_MEMORY)
}
#[cfg(any(systemrisc, feature = "test-support"))]
impl Manager
{
    pub(super) fn scan_request(&mut self, call: &mut FileScanCall) -> Result<(), Status>
    {
        if call.operation != SCAN_DIRECTORY || call.directory < ROOT_ID
        {
            return Err(BAD_ARGUMENT);
        }
        // The Pipes folder appends registry rows after its real catalog
        // children, so its continuation also carries the registry revision and
        // the row offset. Every other directory leaves both fields zero.
        let registry = self.pipes_scan_revision(call.volume, call.directory);
        if registry.is_none() && (call.cursor.reserved != 0 || call.cursor.registry != 0)
        {
            return Err(BAD_ARGUMENT);
        }
        let mounted = self.mounted(call.volume)?;
        if mounted.poisoned
        {
            return Err(ROLLBACK_FAILED);
        }
        let mut cursor = call.cursor;
        if cursor.revision == 0
        {
            if cursor != DirectoryScanCursor::default()
            {
                return Err(BAD_ARGUMENT);
            }
            cursor.revision = mounted.catalog_revision;
            cursor.volume = call.volume;
            cursor.directory = call.directory;
            cursor.reserved = registry.unwrap_or(0);
        }
        else
        {
            if cursor.volume != call.volume || cursor.directory != call.directory
            {
                return Err(BAD_ARGUMENT);
            }
            if cursor.revision != mounted.catalog_revision
            {
                return Err(SCAN_STALE);
            }
            // A registration or destruction while this enumeration is open
            // invalidates the row offsets already handed out.
            if registry.is_some_and(|revision| cursor.reserved != revision)
            {
                return Err(SCAN_STALE);
            }
        }
        let scanner = mounted.scanner.as_ref().ok_or(NOT_SUPPORTED)?;
        let mut position = Cursor { node:      cursor.node,
                                    previous:  cursor.previous,
                                    index:     cursor.index,
                                    traversed: cursor.traversed,
                                    state:     cursor.state };
        let result =
            scanner.step(&mut mounted.device, call.directory, &mut position).map_err(|error| {
                                                                                 match error {
            ScanError::InvalidCursor=>BAD_ARGUMENT,
            ScanError::NoMemory=>NO_MEMORY,
            ScanError::Catalog(_)=>IO_ERROR
        }
                                                                             })?;
        call.entry = CatalogInfo::default();
        call.data_allocated = 0;
        call.resource_allocated = 0;
        call.step = match result
        {
            Step::Entry(entry) =>
            {
                call.data_allocated = entry.data_allocated;
                call.resource_allocated = entry.resource_allocated;
                call.entry = metadata(call.volume, &entry);
                SCAN_ENTRY
            }
            Step::Pending => SCAN_PENDING,
            // Real children come first; registry rows begin only once the
            // catalog portion of this folder is exhausted.
            Step::Done if registry.is_some() =>
            {
                match self.pipes_row(call.volume, call.directory, cursor.registry)
                {
                    Some(entry) =>
                    {
                        call.entry = entry;
                        cursor.registry = cursor.registry.checked_add(1).ok_or(BAD_ARGUMENT)?;
                        SCAN_ENTRY
                    }
                    None => SCAN_DONE
                }
            }
            Step::Done => SCAN_DONE
        };
        cursor.node = position.node;
        cursor.previous = position.previous;
        cursor.index = position.index;
        cursor.traversed = position.traversed;
        cursor.state = position.state;
        call.cursor = cursor;
        Ok(())
    }
}
pub(super) fn writes_catalog(operation: u32, index: u32) -> bool
{
    matches!(operation,
             NEW_FOLDER
             | MOVE_TO_TRASH
             | PUT_AWAY
             | EMPTY_TRASH
             | RENAME
             | REMOVE
             | SET_PROPERTIES
             | SET_LOCKED
             | SET_STATIONERY
             | SET_FOLDER_STATE
             | SET_ICON_LOCATION
             | MOVE_INTO
             | SET_COMMENT)
    || (matches!(operation, TRASH_DIRECTORY | FIND_FOLDER) && index != 0)
}
