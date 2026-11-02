//! Owned, generation-checked native file continuations. No caller span survives
//! admission, and a device is returned before the original thread can wake.
use super::{super::objects::Payload, *};
use crate::context::{DeferredIo, ThreadId};

static ACTIVE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static PUMPING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub(super) struct Source
{
    pub(super) volume:      u32,
    pub(super) node:        core::sync::atomic::AtomicU32,
    opening:                bool,
    pub(super) description: Arc<OpenDescription>,
    disk:                   u32,
    partition:              Partition
}
impl Source
{
    fn conflicts(&self, other: &Self) -> bool
    {
        Arc::ptr_eq(&self.description, &other.description)
        || self.disk == other.disk
           && self.partition == other.partition
           && (self.node.load(Ordering::Acquire) == 0
               || other.node.load(Ordering::Acquire) == 0
               || self.node.load(Ordering::Acquire) == other.node.load(Ordering::Acquire))
           && self.description.kind == other.description.kind
    }
}

struct Packet
{
    input:   Arc<Input>,
    subject: Subject,
    source:  Source,
    record:  spinning_top::Spinlock<Record>
}
struct Record
{
    flight:        Option<Arc<Operation>>,
    terminal:      Option<Status>,
    abandoned:     bool,
    started:       bool,
    position:      u64,
    length:        u64,
    transferred:   usize,
    output:        Option<Vec<u8>>,
    open:          Option<open::Open>,
    open_complete: bool,
    catalog:       Option<CatalogInfo>,
    handle:        u32
}
pub(in crate::toolbox::files) struct Entry
{
    thread:    ThreadId,
    owner:     u64,
    authority: OwnerRef,
    packet:    Arc<Packet>
}
impl Entry
{
    pub(in crate::toolbox::files) fn protects(&self, manager: &Manager, volume: u32, node: u32)
                                              -> bool
    {
        self.packet.source.node.load(Ordering::Acquire) == node && self.uses(manager, volume)
    }

    pub(in crate::toolbox::files) fn uses(&self, manager: &Manager, volume: u32) -> bool
    {
        let source = &self.packet.source;
        let same = match manager.volumes.get(volume)
        {
            Some(Mounted::Block(mounted)) =>
            {
                mounted.device.index == source.disk && mounted.partition == source.partition
            }
            Some(Mounted::Operating(operation)) =>
            {
                operation.disk == source.disk && operation.partition == source.partition
            }
            _ => false
        };
        same && self.packet.record.lock().terminal.is_none()
    }
}
impl Payload for Entry
{
    type Storage = super::super::objects::storage::Native<Self>;

    const KIND: crate::toolbox::handles::KindCode = crate::toolbox::handles::KindCode(*b"FIOP");

    fn cleanup_pending(&self) -> bool
    {
        self.packet.record.lock().terminal.is_none()
    }

    fn begin_cleanup(&self)
    {
        self.packet.record.lock().abandoned = true;
        ACTIVE.store(true, Ordering::Release);
    }

    fn owner_stopping(&self) -> bool
    {
        !self.authority.accepting()
    }

    fn access_allowed(&self, access: subject::Access<'_>) -> bool
    {
        matches!(access,subject::Access::Client(owner) if owner==self.owner)
    }
    // The operation owns its DIOP root, not a lease that waits for itself.
}
impl lifetime::OperationHost for Manager
{
    fn release_file_operation(&mut self, ticket: mac_object_table::domain::ReleasedResource,
                              storage: bool)
                              -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        self.operations.release(ticket, storage)
    }
}
fn files<R>(f: impl FnOnce(&mut Manager) -> Result<R, Status>) -> Result<R, Status>
{
    crate::kernel::with_files(f).map_err(|error| error.status())?
}

pub(in crate::toolbox::files) fn has_waiter(manager: &Manager, thread: ThreadId) -> bool
{
    manager.operations.iter().any(|(_, entry)| entry.thread == thread)
}

pub(crate) fn io_pending(thread: ThreadId, handle: u64) -> Result<bool, Status>
{
    files(|manager| {
        let entry = manager.operations.completing(handle).ok_or(NOT_FOUND)?;
        if entry.thread != thread
        {
            return Err(ACCESS_DENIED);
        }
        Ok(entry.packet.record.lock().terminal.is_none())
    })
}
pub(crate) fn abandon(thread: ThreadId, handle: Option<u64>)
{
    let _ = files(|manager| {
        for (id, entry) in manager.operations.iter()
        {
            if entry.thread == thread && handle.is_none_or(|handle| id == handle)
            {
                entry.begin_cleanup();
            }
        }
        Ok(())
    });
}

pub(in crate::toolbox::files) fn request(owner: u64, authority: &OwnerRef, call: &mut FileCall,
                                         bytes: &[u8], output: &mut [u8])
                                         -> Status
{
    request_input(owner, authority, call, "", bytes, output)
}
pub(super) fn request_open(owner: u64, authority: &OwnerRef, call: &mut FileCall, name: &str)
                           -> Status
{
    request_input(owner, authority, call, name, &[], &mut [])
}
fn request_input(owner: u64, authority: &OwnerRef, call: &mut FileCall, name: &str, bytes: &[u8],
                 output: &mut [u8])
                 -> Status
{
    if let Err(error) = crate::context::current_thread_context().traps.check_defer()
    {
        return error;
    }
    let thread = crate::context::current_thread();
    let mut key = Call::from(&*call);
    if key.operation == OPEN_OPTIONS && key.open_flags & OPEN_APPEND != 0
    {
        key.open_flags |= OPEN_WRITE;
    }
    let existing = files(|manager| {
        manager.validate_file_authority(owner, authority)?;
        let found = manager.operations.iter().find(|(_, entry)| entry.thread == thread);
        match found
        {
            Some((id, entry))
                if entry.owner == owner
                   && entry.authority.owner() == authority.owner()
                   && entry.packet.input.call == key
                   && entry.packet.input.name == name
                   && entry.packet.input.bytes == bytes
                   && !entry.packet.record.lock().abandoned =>
            {
                Ok(Some((id, entry.packet.clone())))
            }
            Some((_, entry)) =>
            {
                entry.begin_cleanup();
                Err(BAD_ABI)
            }
            None =>
            {
                if key.operation != OPEN_OPTIONS
                {
                    manager.preflight_local_file(owner, authority, &key)?;
                }
                else if manager.read_provider(key.volume)?.is_some()
                {
                    return Err(NOT_SUPPORTED);
                }
                Ok(None)
            }
        }
    });
    let (id, packet) = match existing
    {
        Ok(Some(existing)) => existing,
        Err(error) => return error,
        Ok(None) =>
        {
            let subject =
                match crate::drivers::jobs::subjects::resident().and_then(|operations| {
                          operations.admit_current(&subject::Authority::Client(authority.clone()))
                      })
                {
                    Ok(subject) => subject,
                    Err(error) => return error
                };
            let when = match crate::machine::clock::now().and_then(|now| {
                                                             now.mac_microseconds()
                                                                .ok_or(BAD_ARGUMENT)
                                                         })
            {
                Ok(when) => when,
                Err(error) => return error
            };
            let mut owned = Vec::new();
            if owned.try_reserve_exact(bytes.len()).is_err()
            {
                return NO_MEMORY;
            }
            owned.extend_from_slice(bytes);
            let mut owned_name = String::new();
            if owned_name.try_reserve_exact(name.len()).is_err()
            {
                return NO_MEMORY;
            }
            owned_name.push_str(name);
            let input = match Arc::try_new(Input { call: key,
                                                   name: owned_name,
                                                   bytes: owned,
                                                   when })
            {
                Ok(input) => input,
                Err(_) => return NO_MEMORY
            };
            let open = if key.operation == OPEN_OPTIONS
            {
                match open::Open::prepare(authority, &subject, &key)
                {
                    Ok(open) => Some(open),
                    Err(error) => return error
                }
            }
            else
            {
                None
            };
            let source = match files(|manager| {
                      let (volume, node, description) = if let Some(open) = &open
                      {
                          (key.volume, 0, open.description.clone())
                      }
                      else
                      {
                          manager.preflight_local_file(owner, authority, &key)?;
                          let file =
                              manager.slot_access(subject::Access::Client(owner), key.handle)?;
                          (file.volume, file.id, file.description.clone())
                      };
                      if matches!(key.operation, READ | SEEK | TELL)
                      {
                          manager.validate_transfer_read(owner, volume, node)?;
                      }
                      else
                      {
                          manager.validate_transfer_conflicts(owner, volume)?;
                          if manager.writes.iter().any(|(_, mark)| mark.volume == volume)
                             || manager.streams.iter().any(|(_, mark)| mark.volume == volume)
                             || manager.groups.iter().any(|(_, mark)| mark.volume == volume)
                          {
                              return Err(SERVICE_BUSY);
                          }
                      }
                      let mounted = manager.mounted(volume)?;
                      Ok(Source { volume,
                                  node: core::sync::atomic::AtomicU32::new(node),
                                  opening: key.operation == OPEN_OPTIONS,
                                  description,
                                  disk: mounted.device.index,
                                  partition: mounted.partition })
                  })
            {
                Ok(source) => source,
                Err(error) => return error
            };
            // Keep the outer Arc across canonical insertion. Refused insertion
            // cannot drop the last permit/description owner under Files.
            let packet=match Arc::try_new(Packet {input,subject,source,record:spinning_top::Spinlock::new(Record {flight:None,terminal:None,abandoned:false,started:false,position:0,length:0,transferred:0,output:None,open,open_complete:false,catalog:None,handle:0})}) {Ok(packet)=>packet,Err(_)=>return NO_MEMORY};
            let kinds = match crate::toolbox::handles::file_operation_kinds()
            {
                Ok(kinds) => kinds,
                Err(error) => return error
            };
            let id = match files(|manager| {
                      manager.validate_file_authority(owner, authority)?;
                      if key.operation != OPEN_OPTIONS
                      {
                          manager.preflight_local_file(owner, authority, &key)?;
                      }
                      manager.operations.prepare(kinds)?;
                      manager.operations.insert(authority,
                                                Entry { thread,
                                                        owner,
                                                        authority: authority.clone(),
                                                        packet: packet.clone() })
                  })
            {
                Ok(id) => id,
                Err(error) => return error
            };
            ACTIVE.store(true, Ordering::Release);
            (id, packet)
        }
    };
    pump();
    let result = {
        let record = packet.record.lock();
        record.terminal.map(|status| (status, record.position, record.length, record.transferred))
    };
    if let Some((status, position, length, transferred)) = result
    {
        let fits =
            packet.record.lock().output.as_ref().is_none_or(|bytes| bytes.len() <= output.len());
        if !fits
        {
            abandon(thread, Some(id));
            return BAD_ABI;
        }
        // ACK the canonical record before modifying any replay-key field or
        // consuming owned output. A refused Files guard retains the result.
        if files(|manager| manager.operations.remove(id).ok_or(NOT_FOUND)).is_err()
        {
            return defer(thread, id);
        }
        let delivered = packet.record.lock().output.take();
        if let Some(bytes) = delivered
        {
            output[..bytes.len()].copy_from_slice(&bytes);
        }
        call.position = position;
        call.length = length;
        call.transferred = transferred;
        if packet.source.opening
        {
            let record = packet.record.lock();
            if let Some(catalog) = record.catalog
            {
                call.cnid = catalog.id;
                call.entry = catalog;
            }
            call.handle = record.handle;
        }
        return status;
    }
    defer(thread, id)
}
fn defer(thread: ThreadId, id: u64) -> Status
{
    match crate::context::current_thread_context().traps.defer(DeferredIo::FileOperation(id))
    {
        Ok(()) => SERVICE_BUSY,
        Err(error) =>
        {
            abandon(thread, Some(id));
            error
        }
    }
}

/// One fixed-owner bounded device step per maintenance sample. The ordinal is
/// a fair scan position only; all authority is the canonical FIOP entry.
pub(crate) fn pump()
{
    if !ACTIVE.load(Ordering::Acquire) || PUMPING.swap(true, Ordering::AcqRel)
    {
        return;
    }
    struct Reset;
    impl Drop for Reset
    {
        fn drop(&mut self)
        {
            PUMPING.store(false, Ordering::Release);
        }
    }
    let _reset = Reset;
    let selected = files(|manager| {
        let count = manager.operations.len();
        if count == 0
        {
            ACTIVE.store(false, Ordering::Release);
            return Ok(None);
        }
        let start = manager.operation_cursor % count;
        let selected = manager.operations
                              .iter()
                              .enumerate()
                              .chain(manager.operations.iter().enumerate())
                              .skip(start)
                              .take(count)
                              .find_map(|(at, (id, entry))| {
                                  let record = entry.packet.record.lock();
                                  let ready = record.terminal.is_none() || record.abandoned;
                                  if !ready
                                  {
                                      return None;
                                  }
                                  // A started operation retains cursor/fork admission between
                                  // steps.
                                  if !record.started
                                     && !record.abandoned
                                     && manager.operations.iter().any(|(other_id, other)| {
                                                                     if id == other_id
                                                                     {
                                                                         return false;
                                                                     }
                                                                     let other_record =
                                                                         other.packet.record.lock();
                                                                     other_record.started
                                                          && other_record.terminal.is_none()
                                                          && entry.packet
                                                                  .source
                                                                  .conflicts(&other.packet.source)
                                                                 })
                                  {
                                      return None;
                                  }
                                  Some((at,
                                        id,
                                        entry.thread,
                                        entry.owner,
                                        entry.authority.clone(),
                                        entry.packet.clone()))
                              });
        if let Some((at, id, thread, owner, authority, packet)) = selected
        {
            manager.operation_cursor = at + 1;
            Ok(Some((id, thread, owner, authority, packet)))
        }
        else
        {
            ACTIVE.store(false, Ordering::Release);
            Ok(None)
        }
    });
    let Ok(Some((id, thread, owner, authority, packet))) = selected
    else
    {
        return;
    };
    let flight = packet.record.lock().flight.clone();
    if flight.is_some()
    {
        if files(|manager| manager.local_ready()).is_err()
        {
            return;
        }
        if !settle(&packet)
        {
            return;
        }
    }
    {
        let mut record = packet.record.lock();
        if (record.abandoned || !authority.accepting()) && record.flight.is_none()
        {
            record.terminal = Some(mac_abi::OPERATION_CANCELLED);
            record.abandoned = true;
        }
    }
    publish_open(&packet, owner, &authority);
    let (terminal, open_complete) = {
        let record = packet.record.lock();
        (record.terminal.is_some(), record.open_complete)
    };
    if !terminal && !open_complete
    {
        // run_owned returns its unconsumed argument on wrong-hart/admission
        // refusal. Never drop a moved device through a fallible context switch.
        let step = crate::machine::io::run_owned(packet.clone(), |packet| {
            crate::context::with_bootstrap_io_subject(Some(packet.subject.clone()), || {
                crate::context::with_allocation(packet.subject.allocation().clone(), || {
                    let work = files(|manager| {
                        manager.begin_local(owner,
                                            &authority,
                                            packet.subject.clone(),
                                            packet.input.clone(),
                                            Some(&packet.source))
                    })?;
                    {
                        let mut record = packet.record.lock();
                        record.started = true;
                        record.flight = Some(work.operation.clone());
                    }
                    work.execute();
                    files(|manager| manager.local_ready())
                })
            })
        });
        if let Ok(Err(error)) = step
        {
            let mut record = packet.record.lock();
            if record.flight.is_none() && error != SERVICE_BUSY
            {
                record.terminal = Some(error);
            }
        }
        // Consume only a real returned device/result, never an elapsed-time ACK.
        settle(&packet);
        publish_open(&packet, owner, &authority);
    }
    let (terminal, abandoned) = {
        let record = packet.record.lock();
        (record.terminal.is_some(), record.abandoned)
    };
    if terminal
    {
        if abandoned
        {
            let handle = packet.record.lock().handle;
            let _ = files(|manager| {
                if handle != 0 && manager.handles.get(handle).is_some()
                {
                    manager.handles.abandon(handle)?;
                }
                manager.operations.remove(id).ok_or(NOT_FOUND)
            });
        }
        else
        {
            crate::machine::scheduler::wake_io(thread.0, DeferredIo::FileOperation(id));
        }
    }
}

fn settle(packet: &Packet) -> bool
{
    let flight = packet.record.lock().flight.clone();
    let Some(flight) = flight
    else
    {
        return true;
    };
    let Some(outcome) = flight.completed.lock().take()
    else
    {
        return false;
    };
    let mut record = packet.record.lock();
    record.flight = None;
    match outcome
    {
        Ok(Reply::Progress { position,
                             length,
                             transferred }) =>
        {
            record.position = position;
            record.length = length;
            record.transferred = transferred;
            let complete = match packet.input.call.operation
            {
                SET_LENGTH => length == packet.input.call.length,
                WRITE => transferred != 0 || packet.input.bytes.is_empty(),
                SYNC | SEEK | TELL => true,
                _ => false
            };
            if complete
            {
                record.terminal = Some(OK);
            }
        }
        Ok(Reply::Read { position,
                         length,
                         bytes }) =>
        {
            record.position = position;
            record.length = length;
            record.transferred = bytes.len();
            record.output = Some(bytes);
            record.terminal = Some(OK);
        }
        Ok(Reply::Open { entry,
                         fork,
                         complete }) =>
        {
            packet.source.node.store(entry.id, Ordering::Release);
            record.catalog = Some(entry);
            record.length = if packet.input.call.fork == RESOURCE_FORK
            {
                entry.resource_length
            }
            else
            {
                entry.data_length
            };
            record.open_complete = complete;
            if complete
            {
                if let Some(fork) = fork
                {
                    *packet.source.description.fork.lock() = FileFork::Block(fork);
                }
                else
                {
                    record.terminal = Some(BAD_ABI);
                }
            }
        }
        Ok(Reply::Directory(_)) => record.terminal = Some(BAD_ABI),
        Err(error) =>
        {
            if let Some(catalog) = *flight.catalog.lock()
            {
                record.catalog = Some(catalog);
                record.length = if packet.input.call.fork == RESOURCE_FORK
                {
                    catalog.resource_length
                }
                else
                {
                    catalog.data_length
                };
                packet.source.node.store(catalog.id, Ordering::Release);
            }
            if let Some((position, length)) = *flight.progress.lock()
            {
                record.position = position;
                record.length = length;
            }
            record.terminal = Some(error);
        }
    }
    true
}

fn publish_open(packet: &Packet, owner: u64, authority: &OwnerRef)
{
    let next = {
        let mut record = packet.record.lock();
        if record.terminal.is_some() || record.abandoned || !record.open_complete
        {
            return;
        }
        let Some(entry) = record.catalog
        else
        {
            record.terminal = Some(BAD_ABI);
            return;
        };
        record.open.take().map(|open| (open, entry))
    };
    let Some((mut open, entry)) = next
    else
    {
        return;
    };
    // Never hold Record or Files across another manager acquisition. The same
    // affine open/permit/value returns to the canonical packet on refusal.
    let result = open.publish(owner, authority, packet.source.volume, entry);
    let mut record = packet.record.lock();
    record.open = Some(open);
    match result
    {
        Ok(handle) =>
        {
            record.handle = handle;
            record.terminal = Some(OK);
        }
        Err(SERVICE_BUSY) if authority.accepting() =>
        {}
        Err(error) => record.terminal = Some(error)
    }
}
