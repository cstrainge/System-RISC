//! Bounded native fork operations with real mounted-device custody. Firmware I/O
//! runs on its fixed-owner transport after releasing the File Manager guard.
#![forbid(unsafe_code)]
use core::sync::atomic::Ordering;

use super::*;
use crate::drivers::jobs::subjects::Subject;
static PENDING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
pub(in crate::toolbox::files) fn needs_cleanup() -> bool
{
    PENDING.load(Ordering::Acquire)
}

pub(super) struct Operation
{
    volume:    u32,
    info:      VolumeInfo,
    _subject:  Subject,
    finished:  spinning_top::Spinlock<Option<Finished>>,
    completed: spinning_top::Spinlock<Option<Result<Reply, Status>>>
}
struct Finished
{
    mounted:  BlockMounted,
    changed:  bool,
    parent:   u32,
    revision: u64,
    reply:    Result<Reply, Status>,
    node:     u32,
    resource: bool
}
impl Operation
{
    pub(super) fn info(&self) -> VolumeInfo
    {
        self.info
    }
}
struct Input
{
    call:  FileCall,
    name:  String,
    bytes: Vec<u8>,
    when:  u64
}
// FileCall's pointer fields are cleared at the checked native boundary. Input
// never dereferences a caller pointer and remains local to the firmware hart.
struct Work
{
    operation:   Arc<Operation>,
    mounted:     BlockMounted,
    input:       Input,
    description: Option<Arc<OpenDescription>>,
    node:        u32,
    protected:   Vec<u32>,
    revision:    u64,
    changed:     bool,
    parent:      u32
}
enum Reply
{
    Open(CatalogEntry),
    Progress
    {
        position:    u64,
        length:      u64,
        transferred: usize
    }
}
fn overlaps(a: Partition, b: Partition) -> bool
{
    a.start < b.start.saturating_add(b.sectors) && b.start < a.start.saturating_add(a.sectors)
}
fn mutation_status(error: MutationError) -> Status
{
    match error
    {
        MutationError::Invalid => BAD_ARGUMENT,
        MutationError::ReadOnly => READ_ONLY,
        MutationError::NotFound => NOT_FOUND,
        MutationError::NotEmpty => DIRECTORY_NOT_EMPTY,
        MutationError::Exists => ALREADY_EXISTS,
        MutationError::Protected => ACCESS_DENIED,
        MutationError::Busy => SERVICE_BUSY,
        MutationError::Full => DISK_FULL,
        MutationError::NoMemory => NO_MEMORY,
        MutationError::Unsupported => NOT_SUPPORTED,
        MutationError::Io => IO_ERROR,
        MutationError::RollbackFailed => ROLLBACK_FAILED
    }
}
fn selected(entry: &mut CatalogEntry, kind: u32) -> Result<Fork, Status>
{
    match kind
    {
        DATA_FORK => entry.data.take(),
        RESOURCE_FORK => entry.resources.take(),
        _ => None
    }.ok_or(NOT_SUPPORTED)
}
impl Work
{
    fn perform(&mut self) -> Result<(Reply, bool, u32), Status>
    {
        let input = &self.input;
        let call = &input.call;
        let mut volume = io(Volume::mount(&mut self.mounted.device, self.mounted.partition))?;
        let mut changed = false;
        let mut entry = if call.operation == OPEN_OPTIONS
        {
            let flags = call.open_flags;
            let existing = if input.name.is_empty()
            {
                io(volume.entry(call.cnid))?
            }
            else
            {
                io(volume.lookup(if call.cnid == 0 { ROOT_ID } else { call.cnid },
                                 input.name.as_bytes()))?
            };
            let mut entry = match existing
            {
                Some(_) if flags & OPEN_CREATE_NEW != 0 => return Err(ALREADY_EXISTS),
                Some(entry) => entry,
                None if flags & (OPEN_CREATE | OPEN_CREATE_NEW) != 0 =>
                {
                    let entry =
                        volume.create_file(if call.cnid == 0 { ROOT_ID } else { call.cnid },
                                           &input.name,
                                           input.when)
                              .map_err(mutation_status)?;
                    self.changed = true;
                    self.mounted.scanner = Some(io(volume.catalog_scanner())?);
                    self.parent = entry.parent;
                    entry
                }
                None => return Err(NOT_FOUND)
            };
            if entry.directory
            {
                return Err(BAD_ARGUMENT);
            }
            if flags & (OPEN_WRITE | OPEN_APPEND) != 0
            {
                // Preflight protection even when neither create nor truncate
                // modifies content during this open.
                if entry.flags & 1 != 0 || self.protected.contains(&entry.id)
                {
                    return Err(ACCESS_DENIED);
                }
                if self.mounted.info.read_only != 0
                {
                    return Err(READ_ONLY);
                }
            }
            if flags & OPEN_TRUNCATE != 0 && flags & OPEN_CREATE_NEW == 0
            {
                entry = volume.set_fork_len(entry.id,
                                            call.fork == RESOURCE_FORK,
                                            0,
                                            input.when,
                                            &self.protected)
                              .map_err(mutation_status)?;
                changed = true;
                self.changed = true;
                self.mounted.scanner = Some(io(volume.catalog_scanner())?);
                self.parent = entry.parent;
            }
            entry
        }
        else
        {
            required(io(volume.entry(self.node))?)?
        };
        let parent = entry.parent;
        self.parent = parent;
        if call.operation == OPEN_OPTIONS
        {
            // Check that this format actually exposes the requested fork.
            if match call.fork
            {
                DATA_FORK => entry.data.is_none(),
                RESOURCE_FORK => entry.resources.is_none(),
                _ => true
            }
            {
                return Err(NOT_SUPPORTED);
            }
            self.mounted.info.free_bytes = volume.free_bytes;
            self.mounted.info.modified = volume.modified;
            return Ok((Reply::Open(entry), changed, parent));
        }
        let description = self.description.as_ref().ok_or(BAD_ABI)?;
        let mut fork = selected(&mut entry, description.kind)?;
        let mut position = description.position.load(Ordering::Acquire);
        let mut transferred = 0;
        match call.operation
        {
            WRITE =>
            {
                if description.flags & (OPEN_WRITE | OPEN_APPEND) == 0
                {
                    return Err(ACCESS_DENIED);
                }
                if description.flags & OPEN_APPEND != 0
                {
                    position = fork.length;
                }
                // A large gap is prepared through bounded SET_LENGTH calls by
                // the std adapter. No zero-length "successful write" advances it.
                if position.saturating_sub(fork.length) > MAX_FILE_TRANSFER as u64
                {
                    return Err(BAD_ARGUMENT);
                }
                if !input.bytes.is_empty()
                {
                    let mut entry = volume.write_fork(self.node,
                                                      description.kind == RESOURCE_FORK,
                                                      position,
                                                      &input.bytes,
                                                      input.when,
                                                      &self.protected)
                                          .map_err(mutation_status)?;
                    fork = selected(&mut entry, description.kind)?;
                    transferred = input.bytes.len();
                    position = position.checked_add(transferred as u64).ok_or(BAD_ABI)?;
                    changed = true;
                    self.changed = true;
                    self.mounted.scanner = Some(io(volume.catalog_scanner())?);
                }
            }
            SET_LENGTH =>
            {
                if description.flags & (OPEN_WRITE | OPEN_APPEND) == 0
                {
                    return Err(ACCESS_DENIED);
                }
                let target = if call.length > fork.length
                {
                    call.length.min(fork.length.saturating_add(MAX_FILE_TRANSFER as u64))
                }
                else
                {
                    call.length
                };
                let mut entry = volume.set_fork_len(self.node,
                                                    description.kind == RESOURCE_FORK,
                                                    target,
                                                    input.when,
                                                    &self.protected)
                                      .map_err(mutation_status)?;
                changed = target != fork.length;
                self.changed |= changed;
                if changed
                {
                    self.mounted.scanner = Some(io(volume.catalog_scanner())?);
                }
                fork = selected(&mut entry, description.kind)?;
            }
            SYNC => volume.sync().map_err(mutation_status)?,
            _ => return Err(BAD_ARGUMENT)
        }
        let length = fork.length;
        *description.fork.lock() = FileFork::Block(fork);
        description.position.store(position, Ordering::Release);
        self.mounted.info.free_bytes = volume.free_bytes;
        self.mounted.info.modified = volume.modified;
        Ok((Reply::Progress { position,
                              length,
                              transferred },
            changed,
            parent))
    }

    fn execute(mut self)
    {
        let result = self.perform();
        let (reply, changed, parent) = match result
        {
            Ok((reply, changed, parent)) => (Ok(reply), changed || self.changed, parent),
            Err(error) =>
            {
                if error == ROLLBACK_FAILED
                {
                    self.mounted.poisoned = true;
                    self.mounted.info.read_only = 1;
                }
                // Preserve actual earlier committed work and the original cause.
                (Err(error), self.changed || error == ROLLBACK_FAILED, self.parent)
            }
        };
        if changed
        {
            self.mounted.catalog_revision = self.revision;
        }
        let node = match &reply
        {
            Ok(Reply::Open(entry)) => entry.id,
            _ => self.node
        };
        let resource =
            self.description.as_ref().map_or(self.input.call.fork == RESOURCE_FORK,
                                             |description| description.kind == RESOURCE_FORK);
        *self.operation.finished.lock() = Some(Finished { mounted: self.mounted,
                                                          changed,
                                                          parent,
                                                          revision: self.revision,
                                                          reply,
                                                          node,
                                                          resource });
    }
}
impl Manager
{
    /// Restore the exact custodied device only after terminal synchronous I/O.
    /// If a Files reacquisition was refused, the canonical slot retains it here.
    pub(super) fn local_ready(&mut self) -> Result<(), Status>
    {
        let Some(operation) = self.local_operation.clone()
        else
        {
            return Ok(());
        };
        let current = self.volumes.get(operation.volume).ok_or(BAD_ABI)?;
        if !matches!(current,Mounted::Operating(actual) if Arc::ptr_eq(actual,&operation))
        {
            return Err(BAD_ABI);
        }
        let Some(finished) = operation.finished.lock().take()
        else
        {
            return Err(SERVICE_BUSY);
        };
        let disk = finished.mounted.device.index;
        let partition = finished.mounted.partition;
        let native_snapshot = match finished.mounted.scanner.as_ref()
        {
            Some(mac_filesystem::scan::CatalogScanner::HfsB(snapshot)) => Some(*snapshot),
            _ => None
        };
        // No fallible action after taking the genuine mounted state.
        let slot = self.volumes.get_mut(operation.volume);
        if let Some(slot) = slot
        {
            *slot = Mounted::Block(finished.mounted);
        }
        else
        {
            *operation.finished.lock() = Some(finished);
            return Err(BAD_ABI);
        }
        *operation.completed.lock() = Some(finished.reply);
        self.local_operation = None;
        PENDING.store(false, Ordering::Release);
        if finished.changed
        {
            // Clear stale aliases of the same actual sectors, preserving
            // unrelated device caches. No callback runs during publication.
            for mounted in self.volumes.values_mut()
            {
                if let Mounted::Block(mounted) = mounted
                {
                    if mounted.device.index == disk && overlaps(mounted.partition, partition)
                    {
                        mounted.device.cache.clear();
                        mounted.catalog_revision = finished.revision;
                        mounted.scanner =
                            native_snapshot.map(mac_filesystem::scan::CatalogScanner::HfsB);
                    }
                }
            }
            if finished.resource
            {
                for (_, file) in self.handles.iter()
                {
                    if file.id == finished.node && file.description.kind == RESOURCE_FORK
                    {
                        let alias=self.volumes.get(file.volume).is_some_and(|mounted|matches!(mounted,Mounted::Block(mounted) if mounted.device.index==disk && mounted.partition==partition));
                        if alias
                        {
                            *file.resource_index.lock() = None;
                        }
                    }
                }
            }
            self.notify_directory_at(operation.volume, finished.parent, finished.revision);
        }
        Ok(())
    }

    fn begin_local(&mut self, owner: u64, authority: &OwnerRef, subject: Subject, input: Input)
                   -> Result<Work, Status>
    {
        self.local_ready()?;
        self.validate_file_authority(owner, authority)?;
        let call = &input.call;
        let (volume, node, description) = if call.operation == OPEN_OPTIONS
        {
            self.validate_creation(subject::Access::Client(owner),
                                   &subject::Authority::Client(authority.clone()),
                                   crate::toolbox::handles::KindCode(*b"FFRK"))?;
            self.prepare_forks(crate::toolbox::handles::file_fork_kinds()?)?;
            (call.volume, call.cnid, None)
        }
        else
        {
            let file = self.slot(owner, call.handle)?;
            if !matches!(&*file.description.fork.lock(), FileFork::Block(_))
            {
                return Err(NOT_SUPPORTED);
            }
            (file.volume, file.id, Some(file.description.clone()))
        };
        self.validate_transfer_mutation(owner, volume)?;
        if self.writes.iter().any(|(_, mark)| mark.volume == volume)
           || self.streams.iter().any(|(_, mark)| mark.volume == volume)
        {
            return Err(SERVICE_BUSY);
        }
        let mounted = self.mounted(volume)?;
        if mounted.poisoned
        {
            return Err(ROLLBACK_FAILED);
        }
        if (call.operation == WRITE
            || call.operation == SET_LENGTH
            || call.operation == OPEN_OPTIONS && call.open_flags & (OPEN_WRITE | OPEN_APPEND) != 0)
           && mounted.info.read_only != 0
        {
            return Err(READ_ONLY);
        }
        let info = mounted.info;
        let disk = mounted.device.index;
        let partition = mounted.partition;
        let mutating = call.operation == WRITE
                       || call.operation == SET_LENGTH
                       || call.operation == OPEN_OPTIONS
                          && call.open_flags & (OPEN_CREATE | OPEN_CREATE_NEW | OPEN_TRUNCATE) != 0;
        if mutating && self.volumes.iter().any(|(_,mount)|matches!(mount,Mounted::Block(other) if other.device.index==disk && other.partition!=partition && overlaps(other.partition,partition))) {
            return Err(NOT_SUPPORTED);
        }
        let mut protected = Vec::new();
        protected.try_reserve_exact(self.pins.len()).map_err(|_| NO_MEMORY)?;
        protected.extend(self.pins.objects().filter(|pin| pin.volume == volume).map(|pin| pin.id));
        if description.is_some() && call.operation != SYNC && protected.contains(&node)
        {
            return Err(ACCESS_DENIED);
        }
        let revision = scan::next_revision()?;
        let operation=Arc::try_new(Operation {volume,info,_subject:subject,finished:spinning_top::Spinlock::new(None),completed:spinning_top::Spinlock::new(None)}).map_err(|_|NO_MEMORY)?;
        let slot = self.volumes.get_mut(volume).ok_or(NOT_FOUND)?;
        let original = core::mem::replace(slot, Mounted::Operating(operation.clone()));
        let mounted = match original
        {
            Mounted::Block(mounted) => mounted,
            other =>
            {
                *slot = other;
                return Err(NOT_SUPPORTED);
            }
        };
        self.local_operation = Some(operation.clone());
        PENDING.store(true, Ordering::Release);
        Ok(Work { operation,
                  mounted,
                  input,
                  description,
                  node,
                  protected,
                  revision,
                  changed: false,
                  parent: 0 })
    }
}

pub(super) fn request(owner: u64, authority: &OwnerRef, call: &mut FileCall, name: &str,
                      bytes: &[u8])
                      -> Status
{
    let mut flags = call.open_flags;
    if call.operation == OPEN_OPTIONS
    {
        if flags & !OPEN_FLAGS != 0
           || flags & (OPEN_READ | OPEN_WRITE | OPEN_APPEND) == 0
           || flags & (OPEN_CREATE | OPEN_CREATE_NEW | OPEN_TRUNCATE) != 0
              && flags & (OPEN_WRITE | OPEN_APPEND) == 0
           || flags & OPEN_TRUNCATE != 0 && flags & OPEN_APPEND != 0 && flags & OPEN_CREATE_NEW == 0
           || !matches!(call.fork, DATA_FORK | RESOURCE_FORK)
           || name.is_empty() && flags & (OPEN_CREATE | OPEN_CREATE_NEW) != 0
           || name.contains('/')
           || name.contains('\0')
           || name == "."
           || name == ".."
        {
            return BAD_ARGUMENT;
        }
        if flags & OPEN_APPEND != 0
        {
            flags |= OPEN_WRITE;
        }
    }
    let subject = match crate::drivers::jobs::subjects::resident().and_then(|operations| {
                            operations.admit_current(&subject::Authority::Client(authority.clone()))
                        })
    {
        Ok(subject) => subject,
        Err(error) => return error
    };
    let when = match crate::machine::clock::now().and_then(|now| {
                                                     now.mac_microseconds().ok_or(BAD_ARGUMENT)
                                                 })
    {
        Ok(when) => when,
        Err(error) => return error
    };
    let mut owned_name = String::new();
    if owned_name.try_reserve_exact(name.len()).is_err()
    {
        return NO_MEMORY;
    }
    owned_name.push_str(name);
    let mut owned_bytes = Vec::new();
    if owned_bytes.try_reserve_exact(bytes.len()).is_err()
    {
        return NO_MEMORY;
    }
    owned_bytes.extend_from_slice(bytes);
    let mut snapshot = *call;
    snapshot.open_flags = flags;
    snapshot.name = core::ptr::null();
    snapshot.name_length = 0;
    snapshot.input = core::ptr::null();
    snapshot.input_length = 0;
    snapshot.output = core::ptr::null_mut();
    snapshot.capacity = 0;
    let input = Input { call: snapshot,
                        name: owned_name,
                        bytes: owned_bytes,
                        when };
    let outcome=crate::context::with_io_subject(subject.clone(),|| {
        crate::machine::io::run(|| {
            let work=crate::kernel::with_files(|manager|manager.begin_local(owner,authority,subject,input)).map_err(|error|error.status())??;
            let operation=work.operation.clone();
            // Fixed-owner I/O remains active; no Files/state lock crosses I/O.
            work.execute();
            let restored=crate::kernel::with_files(|manager|manager.local_ready()).map_err(|error|error.status()).and_then(core::convert::identity);
            if let Err(refusal)=restored {
                // The original mounted device AND terminal result remain in
                // the canonical operation. Publish any real committed prefix.
                let guard=operation.finished.lock();
                let finished=guard.as_ref().ok_or(BAD_ABI)?;
                return match &finished.reply {
                    Err(original)=>Err(*original),
                    Ok(Reply::Progress {position,length,transferred})=>{
                        call.position=*position;call.length=*length;call.transferred=*transferred;Err(refusal)
                    },
                    Ok(Reply::Open(_))=>Err(refusal),
                };
            }
            let outcome=operation.completed.lock().take().ok_or(BAD_ABI)?;
            outcome
        }).map_err(|error|error.status()).and_then(core::convert::identity)
    }).and_then(core::convert::identity);
    match outcome
    {
        Ok(Reply::Progress { position,
                             length,
                             transferred }) =>
        {
            call.position = position;
            call.length = length;
            call.transferred = transferred;
            OK
        }
        Ok(Reply::Open(mut entry)) =>
        {
            let fork = match selected(&mut entry, call.fork)
            {
                Ok(fork) => fork,
                Err(error) => return error
            };
            let length = fork.length;
            match crate::kernel::with_files(|manager| {
                manager.handles.insert_authorized(&subject::Authority::Client(authority.clone()),OpenFile {
                    owner,id:entry.id,volume:call.volume,description:OpenDescription::new(FileFork::Block(fork),call.fork,flags)?,
                    retired:core::sync::atomic::AtomicBool::new(false),last_close:core::sync::atomic::AtomicBool::new(false),operations:core::sync::atomic::AtomicU32::new(0),authority:subject::Authority::Client(authority.clone()),io_subject:None,resource_index:spinning_top::Spinlock::new(None),
                })
            }).map_err(|error|error.status()).and_then(core::convert::identity) {
                Ok(handle)=>{call.handle=handle;call.position=0;call.length=length;call.cnid=entry.id;OK},Err(error)=>error,
            }
        }
        Err(error) => error
    }
}
