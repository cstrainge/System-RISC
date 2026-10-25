//! Provider cursor projection. No index data or borrowed app state is retained
//! between calls; every request is bound to its mounted generation/revision.
#![forbid(unsafe_code)]
use super::*;
impl Manager
{
    pub(in crate::toolbox::files) fn begin_provider_scan(
        &mut self, call: &FileScanCall)
        -> Result<(u64, DirectoryScanCursor), Status>
    {
        if call.operation != SCAN_DIRECTORY || call.directory < ROOT_ID
        {
            return Err(BAD_ARGUMENT);
        }
        let revision = self.provider_mount(call.volume)?.revision;
        let mut cursor = call.cursor;
        if cursor.revision == 0
        {
            if cursor != DirectoryScanCursor::default()
            {
                return Err(BAD_ARGUMENT);
            }
            cursor.revision = revision;
            cursor.volume = call.volume;
            cursor.directory = call.directory;
        }
        else
        {
            if cursor.volume != call.volume || cursor.directory != call.directory
            {
                return Err(BAD_ARGUMENT);
            }
            if cursor.revision != revision
            {
                return Err(SCAN_STALE);
            }
        }
        if cursor.node != 0
           || cursor.previous != 0
           || cursor.traversed != 0
           || cursor.reserved != 0
           || cursor.state > 1
        {
            return Err(BAD_ARGUMENT);
        }
        let provider = self.begin_provider_read(call.volume)?;
        Ok((provider, cursor))
    }
}
pub(super) fn reply(mut cursor: DirectoryScanCursor, bytes: &[u8]) -> Result<FileScanCall, Status>
{
    let header = bytes.get(..4).ok_or(BAD_ABI)?;
    let mut call = FileScanCall { operation: SCAN_DIRECTORY,
                                  volume: cursor.volume,
                                  directory: cursor.directory,
                                  ..Default::default() };
    call.step = match u32::from_le_bytes(header.try_into().map_err(|_| BAD_ABI)?)
    {
        fs::SCAN_PENDING if bytes.len() == 4 =>
        {
            if cursor.state != 0
            {
                return Err(SCAN_STALE);
            }
            SCAN_PENDING
        }
        fs::SCAN_DONE if bytes.len() == 4 =>
        {
            cursor.state = 1;
            SCAN_DONE
        }
        fs::SCAN_ENTRY =>
        {
            if cursor.state != 0
            {
                return Err(SCAN_STALE);
            }
            call.entry = super::catalog(cursor.volume, &bytes[4..], None, Some(cursor.directory))?;
            if !call.entry.is_directory()
            {
                call.data_allocated = ALLOCATION_UNKNOWN;
                call.resource_allocated = ALLOCATION_UNKNOWN;
            }
            cursor.index = cursor.index.checked_add(1).ok_or(BAD_ABI)?;
            SCAN_ENTRY
        }
        _ => return Err(BAD_ABI)
    };
    call.cursor = cursor;
    Ok(call)
}
