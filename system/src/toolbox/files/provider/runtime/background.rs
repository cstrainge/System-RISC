//! Owned, nonparking I/O for resident coordinators. This reuses the same
//! admission/continuation/cleanup paths as public SDK requests, while leaving
//! an application's event loop free to service Stop/Escape and ordinary input.
#![forbid(unsafe_code)]
use super::{super::super::{Volume, io},
            *};

fn key(operation: u32, volume: u32) -> Key
{
    Key { record: Record::File,
          operation,
          volume,
          cnid: 0,
          index: 0,
          handle: 0,
          fork: 0,
          capacity: 0,
          cursor: DirectoryScanCursor::default(),
          resource: None,
          metadata: None,
          when: 0 }
}
fn pending(result: Result<Flight, Status>) -> Progress
{
    match result
    {
        Ok(flight) => Progress::Pending(flight),
        Err(error) => Progress::Done(Err(error))
    }
}
fn buffer(count: usize) -> Result<Vec<u8>, Status>
{
    if count > fs::MAX_TRANSFER
    {
        return Err(BAD_ARGUMENT);
    }
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(count).map_err(|_| NO_MEMORY)?;
    bytes.resize(count, 0);
    Ok(bytes)
}

pub(super) fn file(owner: u64, call: &mut FileCall, text: &str, capacity: usize) -> Progress
{
    call.capacity = capacity;
    let routed: Result<Option<u32>, Status> = files(|m| {
        if matches!(call.operation, VOLUME_INFO | VOLUME_INDEX | VOLUME_COUNT)
        {
            return Ok(None);
        }
        if matches!(call.operation, READ | CLOSE)
        {
            m.file_provider(owner, call.handle)
        }
        else
        {
            m.read_provider(call.volume)
        }
    });
    let provider = match routed
    {
        Ok(Some(provider)) => provider,
        Ok(None) => return immediate_file(owner, call, text, capacity),
        Err(error) => return Progress::Done(Err(error))
    };
    let mut key = key(call.operation, call.volume);
    key.cnid = call.cnid;
    key.index = call.index;
    key.handle = call.handle;
    key.fork = call.fork;
    key.capacity = capacity;
    key.when = call.position;
    key.metadata = super::super::super::properties::key(call);
    match call.operation
    {
        OPEN | READ | CLOSE => forks::begin(owner, 0, key, call, capacity),
        LOOKUP | PATH | TRASH_DIRECTORY =>
        {
            pending(paths::begin(owner,
                                 0,
                                 key,
                                 call,
                                 if call.operation == TRASH_DIRECTORY { "Trash" } else { text }))
        }
        NEW_FOLDER | super::super::super::properties::CREATE_PRIVATE_DIRECTORY =>
        {
            pending(writes::folder_flight(owner, 0, key, call, text))
        }
        RENAME | MOVE_INTO | SET_COMMENT | SET_LOCKED | SET_STATIONERY | SET_ICON_LOCATION
        | SET_FOLDER_STATE | SET_PROPERTIES | REMOVE =>
        {
            key.record = Record::Metadata;
            pending(metadata::begin(owner, 0, key, call, text))
        }
        CATALOG | PARENT | COMMENT | DIRECTORY_ENTRY =>
        {
            let result = (|| {
                files(|m| m.begin_provider_read(call.volume))?;
                let node = if call.cnid == 0 { ROOT_ID } else { call.cnid };
                let (step, operation, args) = match call.operation
                {
                    DIRECTORY_ENTRY =>
                    {
                        (Step::Child { volume: call.volume,
                                       parent: node,
                                       index:  call.index },
                         Operation::Scan,
                         Arguments { volume: call.volume,
                                     parent: node,
                                     offset: u64::from(call.index),
                                     ..Default::default() })
                    }
                    PARENT =>
                    {
                        (Step::Parent { volume: call.volume,
                                        node },
                         Operation::Catalog,
                         Arguments { volume: call.volume,
                                     node,
                                     ..Default::default() })
                    }
                    COMMENT =>
                    {
                        (Step::Comment { volume: call.volume,
                                         node },
                         Operation::Catalog,
                         Arguments { volume: call.volume,
                                     node,
                                     ..Default::default() })
                    }
                    _ =>
                    {
                        (Step::Catalog { volume: call.volume,
                                         node },
                         Operation::Catalog,
                         Arguments { volume: call.volume,
                                     node,
                                     flags: fs::INCLUDE_CHILD_COUNT,
                                     ..Default::default() })
                    }
                };
                let job = match submit_args(provider, operation, args)
                {
                    Ok(job) => job,
                    Err(error) =>
                    {
                        let _ = files(|m| m.end_provider_read(call.volume));
                        return Err(error);
                    }
                };
                Ok(Flight { thread: 0,
                            owner,
                            key,
                            provider,
                            step,
                            job,
                            abandoned: false,
                            input: String::new(),
                            path: None })
            })();
            pending(result)
        }
        super::super::super::properties::RESTORE_MODIFIED =>
        {
            key.record = Record::Metadata;
            pending(metadata::begin(owner, 0, key, call, ""))
        }
        _ => Progress::Done(Err(NOT_SUPPORTED))
    }
}
fn immediate_file(owner: u64, call: &mut FileCall, text: &str, capacity: usize) -> Progress
{
    let result = (|| {
        let mut bytes = buffer(capacity)?;
        if call.operation == super::super::super::properties::CREATE_PRIVATE_DIRECTORY
        {
            files(|m| {
                m.validate_transfer_mutation(owner, call.volume)?;
                let revision = super::super::super::scan::next_revision()?;
                let mounted = m.mounted(call.volume)?;
                if mounted.poisoned
                {
                    return Err(ROLLBACK_FAILED);
                }
                if mounted.info.read_only != 0
                {
                    return Err(READ_ONLY);
                }
                let mut volume = io(Volume::mount(&mut mounted.device, mounted.partition))?;
                let created = volume.create_private_directory(call.cnid, text, call.position)
                                    .map_err(super::super::super::write::status)?;
                call.entry = super::super::super::metadata(call.volume, &created);
                mounted.info.modified = volume.modified;
                mounted.info.free_bytes = volume.free_bytes;
                mounted.catalog_revision = revision;
                mounted.scanner = volume.catalog_scanner().ok();
                m.notify_directory_at(call.volume, call.cnid, revision);
                Ok(())
            })?;
        }
        else if call.operation == super::super::super::properties::RESTORE_MODIFIED
        {
            files(|m| {
                m.validate_transfer_mutation(owner, call.volume)?;
                let revision = super::super::super::scan::next_revision()?;
                let mounted = m.mounted(call.volume)?;
                if mounted.poisoned
                {
                    return Err(ROLLBACK_FAILED);
                }
                if mounted.info.read_only != 0
                {
                    return Err(READ_ONLY);
                }
                let mut volume = io(Volume::mount(&mut mounted.device, mounted.partition))?;
                let restored = volume.restore_modified(call.cnid,
                                                       call.length,
                                                       (call.index != 0).then_some(call.entry
                                                                                       .modified))
                                     .map_err(super::super::super::write::status)?;
                call.entry = super::super::super::metadata(call.volume, &restored);
                mounted.info.modified = volume.modified;
                mounted.info.free_bytes = volume.free_bytes;
                mounted.catalog_revision = revision;
                mounted.scanner = volume.catalog_scanner().ok();
                m.notify_directory_at(call.volume, call.cnid, revision);
                Ok(())
            })?;
        }
        else
        {
            if call.operation == OPEN
            {
                let authority = crate::toolbox::files::creation_authority(owner)?;
                let (handle, length) =
                    files(|m| m.open_for(owner, &authority, call.volume, call.cnid, call.fork))?;
                call.handle = handle;
                call.length = length;
                call.position = 0;
            }
            else
            {
                files(|m| m.request_owned(owner, call, text, &mut bytes))?;
            }
        }
        if call.transferred > bytes.len()
        {
            return Err(BAD_ABI);
        }
        bytes.truncate(call.transferred);
        Ok(match call.operation
        {
            VOLUME_INFO | VOLUME_INDEX => Reply::Mounted(call.volume_info),
            VOLUME_COUNT => Reply::Count(call.index),
            OPEN => Reply::Opened(call.handle, call.length),
            READ =>
            {
                Reply::Read { bytes,
                              position: call.position,
                              length: call.length }
            }
            COMMENT | PATH => Reply::Bytes(bytes),
            CLOSE | REMOVE => Reply::Unmounted,
            _ => Reply::Entry(call.entry)
        })
    })();
    Progress::Done(result)
}

pub(super) fn scan(owner: u64, call: &mut FileScanCall) -> Progress
{
    let provider = match files(|m| m.read_provider(call.volume))
    {
        Ok(Some(provider)) => provider,
        Ok(None) =>
        {
            return Progress::Done(files(|m| m.scan_request(call)).map(|()| Reply::Scan(*call)));
        }
        Err(error) => return Progress::Done(Err(error))
    };
    let mut key = key(SCAN_DIRECTORY, call.volume);
    key.record = Record::Scan;
    key.cnid = call.directory;
    key.cursor = call.cursor;
    let result = (|| {
        let (_, cursor) = files(|m| m.begin_provider_scan(call))?;
        let job = match submit_args(provider,
                                    Operation::Scan,
                                    Arguments { volume: cursor.volume,
                                                parent: cursor.directory,
                                                offset: u64::from(cursor.index),
                                                ..Default::default() })
        {
            Ok(job) => job,
            Err(error) =>
            {
                let _ = files(|m| m.end_provider_read(cursor.volume));
                return Err(error);
            }
        };
        Ok(Flight { thread: 0,
                    owner,
                    key,
                    provider,
                    step: Step::Scan(cursor),
                    job,
                    abandoned: false,
                    input: String::new(),
                    path: None })
    })();
    pending(result)
}

pub(super) fn stream(owner: u64, call: &mut ForkWriteCall, name: &str, comment: &str, data: &[u8],
                     capacity: usize)
                     -> Progress
{
    call.capacity = capacity;
    call.count = data.len();
    let provider = files(|m| {
        if call.operation == BEGIN_FORK_WRITE
        {
            m.read_provider(call.volume)
        }
        else
        {
            m.stream_provider(owner, call.handle)
        }
    });
    match provider
    {
        Ok(Some(_)) =>
        {
            let mut key = key(call.operation, call.volume);
            key.record = Record::Stream;
            key.cnid = call.expected_cnid;
            key.index = call.parent;
            key.handle = call.handle;
            key.fork = call.fork;
            key.capacity = call.count.max(capacity);
            key.when = call.when;
            key.metadata = Some(super::super::super::properties::finder_key(call.finder_info));
            pending(streams::begin(owner, 0, key, call, name, comment, data))
        }
        Ok(None) =>
        {
            let result = (|| {
                let mut bytes = buffer(capacity)?;
                // Only admission needs a new ledger binding. An existing
                // writer already pins its authority; chunks and cleanup use it.
                let authority = if call.operation == BEGIN_FORK_WRITE
                {
                    Some(super::super::super::creation_authority(owner)?)
                }
                else
                {
                    None
                };
                files(|m| {
                    if let Some(authority) = &authority
                    {
                        m.stream_request_for(owner, authority, call, name, comment, data,
                                             &mut bytes)
                    }
                    else
                    {
                        m.stream_request(owner, call, name, comment, data, &mut bytes)
                    }
                })?;
                if call.transferred > bytes.len()
                {
                    return Err(BAD_ABI);
                }
                bytes.truncate(call.transferred);
                Ok(Reply::Stream { handle: call.handle,
                                   completed: call.completed != 0,
                                   entry: call.entry,
                                   bytes })
            })();
            Progress::Done(result)
        }
        Err(error) => Progress::Done(Err(error))
    }
}

pub(super) fn group(owner: u64, call: &mut IconGroupCall, rows: &[IconMove]) -> Progress
{
    let provider = files(|m| {
        if call.operation == BEGIN_ICON_GROUP
        {
            m.read_provider(call.volume)
        }
        else
        {
            m.group_provider(owner, call.handle)
        }
    });
    match provider
    {
        Ok(Some(_)) => {
            let mut key = key(call.operation, call.volume);
            key.record = Record::Group;
            key.index = call.count;
            key.handle = call.handle;
            key.when = call.when;
            pending(groups::begin(owner, 0, key, call, rows))
        }
        Ok(None) => Progress::Done((|| {
            let authority=if call.operation==BEGIN_ICON_GROUP {
                Some(super::super::super::creation_authority(owner)?)
            } else { None };
            files(|m| if let Some(authority)=&authority {
                m.group_request_for(owner,authority,call,rows)
            } else { m.group_request(owner,call,rows) })
        })().and_then(|()| {
            let progress = fs::IconMoveProgress::from_words([call.phase, call.completed, call.count,
                call.changed]).map_err(|_| BAD_ABI)?;
            Ok(Reply::Group {handle:call.handle, progress})
        })),
        Err(error) => Progress::Done(Err(error)),
    }
}
