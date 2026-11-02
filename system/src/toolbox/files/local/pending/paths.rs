//! Process paths retain a genuine directory alias and DIOP root across the same
//! canonical FIOP continuation as file operations. One catalog/lookup device
//! step runs per turn, with no Files/Record guard across firmware callbacks.
#![forbid(unsafe_code)]
use super::{super::super::provider::path::{Builder, component},
            *};

pub(super) struct State
{
    pub(super) input: Arc<Input>,
    at:               usize,
    ancestry:         Builder,
    prefix:           String,
    change:           Option<Arc<pins::DirectoryState>>,
    ready:            bool
}
fn owned(text: &str) -> Result<String, Status>
{
    let mut result = String::new();
    result.try_reserve_exact(text.len()).map_err(|_| NO_MEMORY)?;
    result.push_str(text);
    Ok(result)
}
fn step(volume: u32, node: u32, operation: u32, name: &str) -> Result<Arc<Input>, Status>
{
    let call = FileCall { operation,
                          volume,
                          cnid: node,
                          ..Default::default() };
    Arc::try_new(Input { call:  Call::from(&call),
                         name:  owned(name)?,
                         bytes: Vec::new(),
                         when:  0 }).map_err(|_| NO_MEMORY)
}

pub(in crate::toolbox::files) fn request(owner: u64, authority: &OwnerRef, call: &mut FileCall,
                                         name: &str, output: &mut [u8])
                                         -> Status
{
    match begin(owner, authority, call, name)
    {
        Ok((id, packet)) =>
        {
            let status = finish_request(crate::context::current_thread(), id, packet, call, output);
            if status != OK && status != SERVICE_BUSY
            {
                refused(call.operation, "execution", status)
            }
            else
            {
                status
            }
        }
        Err(status) => refused(call.operation, "admission", status)
    }
}
fn begin(owner: u64, authority: &OwnerRef, call: &FileCall, name: &str)
         -> Result<(u64, Arc<Packet>), Status>
{
    crate::context::current_thread_context().traps.check_defer()?;
    if !matches!(call.operation, CURRENT_DIRECTORY | CHANGE_DIRECTORY | RESOLVE_PATH | OPEN_PATH)
    {
        return Err(NOT_SUPPORTED);
    }
    if name.len() > 4096
       || name.contains('\0')
       || call.operation != CURRENT_DIRECTORY && name.is_empty()
    {
        return Err(BAD_ARGUMENT);
    }
    let thread = crate::context::current_thread();
    let mut key = Call::from(call);
    if key.operation == OPEN_PATH
    {
        let flags = key.open_flags;
        if flags & !OPEN_FLAGS != 0
           || flags & (OPEN_READ | OPEN_WRITE | OPEN_APPEND) == 0
           || flags & (OPEN_CREATE | OPEN_CREATE_NEW | OPEN_TRUNCATE) != 0
              && flags & (OPEN_WRITE | OPEN_APPEND) == 0
           || flags & OPEN_TRUNCATE != 0 && flags & OPEN_APPEND != 0 && flags & OPEN_CREATE_NEW == 0
           || !matches!(key.fork, DATA_FORK | RESOURCE_FORK)
        {
            return Err(BAD_ARGUMENT);
        }
        if flags & OPEN_APPEND != 0
        {
            key.open_flags |= OPEN_WRITE;
        }
    }
    let existing = files(|manager| {
        manager.validate_file_authority(owner, authority)?;
        if let Some((id, entry)) =
            manager.operations.iter().find(|(_, entry)| entry.thread == thread)
        {
            if entry.owner == owner
               && entry.authority.owner() == authority.owner()
               && entry.packet.input.call == key
               && entry.packet.input.name == name
               && !entry.packet.record.lock().abandoned
            {
                return Ok(Some((id, entry.packet.clone())));
            }
            entry.begin_cleanup();
            return Err(BAD_ABI);
        }
        Ok(None)
    })?;
    if let Some(existing) = existing
    {
        return Ok(existing);
    }
    let subject=crate::drivers::jobs::subjects::resident()?.admit_current(&subject::Authority::Client(authority.clone()))
        .map_err(|status|refused(call.operation,"io-subject",status))?;
    let when = crate::machine::clock::now()?.mac_microseconds().ok_or(BAD_ABI)?;
    let input = Arc::try_new(Input { call: key,
                                     name: owned(name)?,
                                     bytes: Vec::new(),
                                     when }).map_err(|_| NO_MEMORY)?;
    let absolute = if name.starts_with('/')
    {
        let (at, namespace) = component(name, 0).ok_or(BAD_ARGUMENT)?;
        if namespace != "Volumes"
        {
            return Err(NOT_FOUND);
        }
        let (at, volume) = component(name, at).ok_or(NOT_FOUND)?;
        if volume == "." || volume == ".."
        {
            return Err(BAD_ARGUMENT);
        }
        Some((at, volume))
    }
    else
    {
        None
    };
    let kinds = crate::toolbox::handles::file_pin_kinds()?;
    let system = crate::toolbox::handles::system_owner()?;
    let (file, info, disk, partition) =
        files(|manager| {
            manager.prepare_executable_pins(system, kinds)?;
            let root = if let Some((_, name)) = absolute
            {
                let mut found = None;
                for (_, volume) in manager.volumes.iter()
                {
                    if let Some(info) = volume.info()
                       && info.name() == name
                    {
                        if found.is_some()
                        {
                            return Err(ALREADY_EXISTS);
                        }
                        found = Some(pins::File { volume: info.id,
                                                  id:     ROOT_ID });
                    }
                }
                Some(found.ok_or(NOT_FOUND)?)
            }
            else
            {
                None
            };
            let file =
                if let Some(root) = root { root } else { manager.pins.directory(authority)? };
            if manager.read_provider(file.volume)?.is_some()
            {
                return Err(NOT_SUPPORTED);
            }
            manager.validate_transfer_read(owner, file.volume, file.id)?;
            let mounted = manager.mounted(file.volume)?;
            let info = mounted.info;
            let disk = mounted.device.index;
            let partition = mounted.partition;
            manager.pins.snapshot_directory(authority, subject.scope().clone(), root)?;
            Ok((file, info, disk, partition))
        }).map_err(|status| {
              serial!("FILE_PATH_SCOPE subject={} owner={} closed={}",
                      subject.scope().subject(),
                      authority.owner(),
                      subject.scope().closed());
              refused(call.operation, "directory-snapshot", status)
          })?;
    if info.name().is_empty() || (info.name().contains('/') || info.name().contains('\0'))
    {
        return Err(NOT_SUPPORTED);
    }
    let mut prefix = owned("/Volumes/")?;
    prefix.try_reserve(info.name().len()).map_err(|_| NO_MEMORY)?;
    prefix.push_str(info.name());
    let open = if key.operation == OPEN_PATH
    {
        Some(open::Open::prepare(authority, &subject, &key)?)
    }
    else
    {
        None
    };
    let description = open.as_ref().map(|open| open.description.clone());
    let state = State { input: step(file.volume, file.id, CATALOG, "")?,
                        at: absolute.map_or(0, |(at, _)| at),
                        ancestry: Builder::default(),
                        prefix,
                        change: if call.operation == CHANGE_DIRECTORY
                        {
                            Some(Arc::try_new(pins::DirectoryState::new()).map_err(|_| NO_MEMORY)?)
                        }
                        else
                        {
                            None
                        },
                        ready: false };
    let packet=Arc::try_new(Packet {input,subject,source:Source {volume:file.volume,node:core::sync::atomic::AtomicU32::new(file.id),opening:key.operation==OPEN_PATH,description,disk,partition},
        record:spinning_top::Spinlock::new(Record {flight:None,terminal:None,abandoned:false,started:false,position:0,length:0,transferred:0,output:None,open,open_complete:false,catalog:None,handle:0,path:Some(state)})}).map_err(|_|NO_MEMORY)?;
    let kinds = crate::toolbox::handles::file_operation_kinds()?;
    let id = files(|manager| {
        manager.validate_file_authority(owner, authority)?;
        if manager.operations.iter().any(|(_, entry)| entry.thread == thread)
        {
            return Err(SERVICE_BUSY);
        }
        manager.operations.prepare(kinds)?;
        manager.operations.insert(authority,
                                  Entry { thread,
                                          owner,
                                          authority: authority.clone(),
                                          packet: packet.clone() })
    })?;
    ACTIVE.store(true, Ordering::Release);
    Ok((id, packet))
}

pub(super) fn consume(packet: &Packet, entry: CatalogInfo)
{
    let state = packet.record.lock().path.take();
    let Some(mut state) = state
    else
    {
        packet.record.lock().terminal = Some(BAD_ABI);
        return;
    };
    let outcome = (|| {
        if entry.volume != packet.source.volume
        {
            return Err(BAD_ABI);
        }
        if state.input.call.operation == CATALOG && entry.id != state.input.call.cnid
           || state.input.call.operation == LOOKUP && entry.parent != state.input.call.cnid
        {
            return Err(BAD_ABI);
        }
        packet.source.node.store(entry.id, Ordering::Release);
        if packet.input.call.operation == CURRENT_DIRECTORY
        {
            if !entry.is_directory()
            {
                return Err(BAD_ABI);
            }
            if entry.id == ROOT_ID
            {
                let ancestry = core::mem::take(&mut state.ancestry).finish()?;
                let suffix = if ancestry == b"/" { &[][..] } else { &ancestry };
                let size = state.prefix.len().checked_add(suffix.len()).ok_or(BAD_ARGUMENT)?;
                if size > 4096
                {
                    return Err(BAD_ARGUMENT);
                }
                let mut bytes = Vec::new();
                bytes.try_reserve_exact(size).map_err(|_| NO_MEMORY)?;
                bytes.extend_from_slice(state.prefix.as_bytes());
                bytes.extend_from_slice(suffix);
                return Ok(Some(bytes));
            }
            state.ancestry.push(&entry)?;
            state.input = step(entry.volume, entry.parent, CATALOG, "")?;
            return Ok(None);
        }
        while let Some((at, part)) = component(&packet.input.name, state.at)
        {
            state.at = at;
            if !entry.is_directory()
            {
                return Err(BAD_ARGUMENT);
            }
            if part == "." || part == ".." && entry.id == ROOT_ID
            {
                continue;
            }
            if packet.input.call.operation == OPEN_PATH
               && part != ".."
               && component(&packet.input.name, at).is_none()
            {
                if packet.input.name.ends_with('/')
                {
                    return Err(BAD_ARGUMENT);
                }
                let mut call = packet.input.call;
                call.operation = OPEN_OPTIONS;
                call.volume = entry.volume;
                call.cnid = entry.id;
                packet.source.node.store(0, Ordering::Release);
                state.input =
                    Arc::try_new(Input { call,
                                         name: owned(part)?,
                                         bytes: Vec::new(),
                                         when: packet.input.when }).map_err(|_| NO_MEMORY)?;
                return Ok(None);
            }
            state.input = if part == ".."
            {
                step(entry.volume, entry.parent, CATALOG, "")?
            }
            else
            {
                step(entry.volume, entry.id, LOOKUP, part)?
            };
            return Ok(None);
        }
        if (packet.input.call.operation == CHANGE_DIRECTORY || packet.input.name.ends_with('/'))
           && !entry.is_directory()
        {
            return Err(BAD_ARGUMENT);
        }
        if packet.input.call.operation == OPEN_PATH
        {
            return Err(BAD_ARGUMENT);
        }
        state.ready = true;
        Ok(None)
    })();
    let mut record = packet.record.lock();
    record.catalog = Some(entry);
    record.path = Some(state);
    match outcome
    {
        Err(status) => record.terminal = Some(status),
        Ok(Some(bytes)) =>
        {
            record.transferred = bytes.len();
            record.output = Some(bytes);
            record.terminal = Some(OK);
        }
        Ok(None) =>
        {
            if record.path.as_ref().is_some_and(|state| state.ready)
               && packet.input.call.operation == RESOLVE_PATH
            {
                record.terminal = Some(OK);
            }
        }
    }
}

pub(super) fn publish(packet: &Packet, authority: &OwnerRef)
{
    let next = {
        let record = packet.record.lock();
        if record.terminal.is_some() || record.abandoned
        {
            return;
        }
        let Some(state) = record.path.as_ref()
        else
        {
            return;
        };
        if !state.ready
        {
            return;
        }
        record.catalog.zip(state.change.clone())
    };
    let Some((entry, state)) = next
    else
    {
        return;
    };
    let result = files(|manager| {
        manager.validate_file_authority(authority.owner().incarnation(), authority)?;
        manager.pins.replace_directory(authority,
                                       pins::File { volume: entry.volume,
                                                    id:     entry.id },
                                       packet.subject.scope().clone(),
                                       state)
    });
    let mut record = packet.record.lock();
    match result
    {
        Ok(()) => record.terminal = Some(OK),
        Err(SERVICE_BUSY) => (),
        Err(status) => record.terminal = Some(status)
    }
}

fn refused(operation: u32, phase: &str, status: Status) -> Status
{
    serial!("FILE_PATH_REFUSED operation={operation} phase={phase} status={status}");
    status
}
