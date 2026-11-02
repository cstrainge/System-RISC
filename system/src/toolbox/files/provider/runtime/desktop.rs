//! Native combined view over real per-volume folders, not another catalog.
use super::*;

pub(super) fn prepare(call: &mut FileScanCall) -> Result<(), Status>
{
    if call.operation != SCAN_DESKTOP || call.desktop.state > 3
    {
        return Err(BAD_ARGUMENT);
    }
    if call.desktop.state == 0
    {
        let next = files(|m| {
            Ok(m.volumes
                .iter()
                .filter_map(|(_, v)| v.info())
                .map(|v| v.id)
                .filter(|id| *id > call.desktop.volume)
                .min())
        })?;
        if let Some(volume) = next
        {
            call.desktop.volume = volume;
            call.desktop.state = 1;
            call.desktop.directory = DirectoryScanCursor::default();
        }
        else
        {
            call.desktop.state = 3;
        }
    }
    call.volume = call.desktop.volume;
    call.directory = call.desktop.directory.directory;
    call.cursor = call.desktop.directory;
    call.entry = CatalogInfo::default();
    call.data_allocated = 0;
    call.resource_allocated = 0;
    call.step = SCAN_PENDING;
    Ok(())
}

pub(super) fn lookup(call: &FileScanCall) -> FileCall
{
    FileCall { operation: FIND_FOLDER,
               volume: call.desktop.volume,
               file_type: *b"desk",
               ..Default::default() }
}

pub(super) fn finish(mut call: FileScanCall, result: Result<Reply, Status>)
                     -> Result<Reply, Status>
{
    match (call.desktop.state, result)
    {
        (1, Err(NOT_FOUND)) => call.desktop.state = 0,
        (1, Ok(Reply::Entry(entry))) =>
        {
            if entry.volume != call.desktop.volume || !entry.is_directory()
            {
                return Err(BAD_ABI);
            }
            call.desktop.directory.directory = entry.id;
            call.desktop.state = 2;
        }
        (2, Ok(Reply::Scan(reply))) =>
        {
            call.step = reply.step;
            call.entry = reply.entry;
            call.data_allocated = reply.data_allocated;
            call.resource_allocated = reply.resource_allocated;
            call.desktop.directory = reply.cursor;
            if reply.step == SCAN_DONE
            {
                call.desktop.state = 0;
                call.desktop.directory = DirectoryScanCursor::default();
                call.step = SCAN_PENDING;
            }
        }
        (3, Ok(_)) => call.step = SCAN_DONE,
        (_, Err(error)) => return Err(error),
        _ => return Err(BAD_ABI)
    }
    Ok(Reply::Scan(call))
}

pub(super) fn begin(owner: crate::toolbox::files::subject::Access<'_>, call: &mut FileScanCall)
                    -> Progress
{
    if let Err(error) = prepare(call)
    {
        return Progress::Done(Err(error));
    }
    match call.desktop.state
    {
        1 => background::metadata(owner, &mut lookup(call), "", 0),
        2 =>
        {
            let mut input = FileScanCall { operation: SCAN_DIRECTORY,
                                           volume: call.volume,
                                           directory: call.directory,
                                           cursor: if call.cursor.revision == 0
                                           {
                                               DirectoryScanCursor::default()
                                           }
                                           else
                                           {
                                               call.cursor
                                           },
                                           ..Default::default() };
            background::scan(owner, &mut input)
        }
        3 => Progress::Done(Ok(Reply::Scan(*call))),
        _ => Progress::Done(Err(BAD_ABI))
    }
}
