//! Multiplexed callback state machine. All I/O state is owned, every callback
//! advances one bounded step, and no state guard reaches a lower service.
use alloc::vec::Vec;

use mac_driver::{Completion, Error, Phase, Request, Result};
use mac_filesystem_interface::{self as fs, Arguments, Operation};
use mac_sit::{Contents, IndexBuilder, Limits};
use spinning_top::Spinlock;

use crate::{Sources, buffer,
            catalog::{Root, Volume},
            format_error,
            stream::{Fetch, Stream}};
const MAX_MOUNTS: usize = 16;
const MAX_OPENS: usize = 16;
const MAX_ACTIVE: usize = 32;
struct Open
{
    volume: u32,
    token:  u64,
    node:   u32,
    stream: Option<(u32, Stream)>
}
struct Backend<S>
{
    sources: S,
    volumes: Vec<Volume>,
    opens:   Vec<Open>
}
impl<S: Sources> Backend<S>
{
    fn volume(&self, id: u32) -> Result<&Volume>
    {
        self.volumes.iter().find(|v| v.token == id).ok_or(Error(mac_abi::NOT_FOUND))
    }

    fn start(&mut self, op: Operation, a: Arguments<'_>) -> Result<Task>
    {
        if op == Operation::Mount
        {
            if self.volumes.iter().any(|v| v.token == a.volume)
            {
                return Err(Error(mac_abi::ALREADY_EXISTS));
            }
            if self.volumes.len() == MAX_MOUNTS
            {
                return Err(Error(mac_abi::NO_MEMORY));
            }
            crate::module_reserve(&mut self.volumes, 1)?;
            let source = a.auxiliary;
            if source <= u32::MAX as u64
            {
                return Err(Error(mac_abi::BAD_ARGUMENT));
            }
            let root = crate::module_allocation(|| Root::decode(a.payload))?;
            return Ok(Task::Mount(Mounting { volume: a.volume,
                                             source,
                                             root: Some(root),
                                             builder: None,
                                             fetch: None }));
        }
        let volume = self.volume(a.volume)?;
        let bytes = match op
        {
            Operation::Catalog => volume.encode(a.node)?,
            Operation::Lookup =>
            {
                let id =
                    volume.lookup(a.parent, a.name().map_err(|_| Error(mac_abi::BAD_ARGUMENT))?)?;
                volume.encode(id)?
            }
            Operation::Scan =>
            {
                let children = volume.directory(a.parent)?;
                let mut out = buffer(4 + fs::MAX_METADATA_BYTES)?;
                if let Some(&node) = usize::try_from(a.offset).ok().and_then(|i| children.get(i))
                {
                    out[..4].copy_from_slice(&fs::SCAN_ENTRY.to_le_bytes());
                    let count = volume.metadata(node)?
                                      .encode(&mut out[4..])
                                      .map_err(|_| Error(mac_abi::BAD_ABI))?;
                    out.truncate(4 + count);
                }
                else
                {
                    out[..4].copy_from_slice(&fs::SCAN_DONE.to_le_bytes());
                    out.truncate(4);
                }
                out
            }
            Operation::Open =>
            {
                let node = volume.index.get(a.node).ok_or(Error(mac_abi::NOT_FOUND))?;
                let Contents::File(file) = node.contents
                else
                {
                    return Err(Error(mac_abi::BAD_ARGUMENT));
                };
                // Catalogs remain available for encrypted/unsupported entries;
                // Open explicitly rejects them rather than returning zero data.
                file.data.method().map_err(format_error)?;
                file.resource.method().map_err(format_error)?;
                if self.opens.iter().any(|f| f.token == a.token)
                {
                    return Err(Error(mac_abi::ALREADY_EXISTS));
                }
                if self.opens.len() == MAX_OPENS
                {
                    return Err(Error(mac_abi::NO_MEMORY));
                }
                let mut bytes = buffer(16)?;
                bytes[..8].copy_from_slice(&file.data.length.to_le_bytes());
                bytes[8..].copy_from_slice(&file.resource.length.to_le_bytes());
                crate::module_reserve(&mut self.opens, 1)?;
                self.opens.push(Open { volume: a.volume,
                                       token:  a.token,
                                       node:   a.node,
                                       stream: None });
                bytes
            }
            Operation::Read =>
            {
                let open = self.opens
                               .iter()
                               .position(|f| f.volume == a.volume && f.token == a.token)
                               .ok_or(Error(mac_abi::NOT_FOUND))?;
                let node =
                    volume.index.get(self.opens[open].node).ok_or(Error(mac_abi::NOT_FOUND))?;
                let Contents::File(file) = node.contents
                else
                {
                    return Err(Error(mac_abi::BAD_ABI));
                };
                let source = volume.source;
                let fork = if a.flags == fs::DATA_FORK { file.data } else { file.resource };
                let remaining =
                    fork.length.checked_sub(a.offset).ok_or(Error(mac_abi::BAD_ARGUMENT))?;
                let output = buffer(remaining.min(a.length) as usize)?;
                let reusable =
                    self.opens[open].stream.as_ref().is_some_and(|(kind, s)| {
                                                        *kind == a.flags && s.position <= a.offset
                                                    });
                let stream = if reusable
                {
                    self.opens[open].stream.take().unwrap().1
                }
                else
                {
                    crate::module_allocation(|| Stream::new(fork))?
                };
                return Ok(Task::Read(Reading { volume: a.volume,
                                               token: a.token,
                                               kind: a.flags,
                                               source,
                                               offset: a.offset,
                                               stream: Some(stream),
                                               output,
                                               written: 0 }));
            }
            Operation::Close =>
            {
                let at = self.opens
                             .iter()
                             .position(|f| f.volume == a.volume && f.token == a.token)
                             .ok_or(Error(mac_abi::NOT_FOUND))?;
                self.opens.swap_remove(at);
                Vec::new()
            }
            Operation::Unmount =>
            {
                if self.opens.iter().any(|f| f.volume == a.volume)
                {
                    return Err(Error(mac_abi::SERVICE_BUSY));
                }
                let at = self.volumes
                             .iter()
                             .position(|v| v.token == a.volume)
                             .ok_or(Error(mac_abi::NOT_FOUND))?;
                self.volumes.swap_remove(at);
                Vec::new()
            }
            Operation::Refresh => Vec::new(), // Immutable, valid watch target.
            _ => return Err(Error(mac_abi::READ_ONLY))
        };
        Ok(Task::Ready(Ok(bytes)))
    }
}
struct Mounting
{
    volume:  u32,
    source:  u64,
    root:    Option<Root>,
    builder: Option<IndexBuilder>,
    fetch:   Option<Fetch>
}
struct Reading
{
    volume:  u32,
    token:   u64,
    kind:    u32,
    source:  u64,
    offset:  u64,
    stream:  Option<Stream>,
    output:  Vec<u8>,
    written: usize
}
enum Task
{
    Mount(Mounting),
    Read(Reading),
    Ready(Result<Vec<u8>>)
}
impl Task
{
    fn advance<S: Sources>(&mut self, backend: &mut Backend<S>, cancel: bool)
                           -> Result<Option<Vec<u8>>>
    {
        match self
        {
            Self::Ready(_) => unreachable!(),
            Self::Mount(m) =>
            {
                if let Some(fetch) = &mut m.fetch
                {
                    if !fetch.step(&mut backend.sources, m.source, cancel)?
                    {
                        return Ok(None);
                    }
                    crate::module_allocation(|| {
                        m.builder
                         .as_mut()
                         .ok_or(Error(mac_abi::BAD_ABI))?
                         .feed(&fetch.bytes)
                         .map_err(format_error)
                    })?;
                    m.fetch = None;
                    return Ok(None);
                }
                if cancel
                {
                    return Err(Error(mac_abi::OPERATION_CANCELLED));
                }
                if m.builder.is_none()
                {
                    let length = backend.sources.length(m.source)?;
                    m.builder = Some(crate::module_allocation(|| {
                                         IndexBuilder::new(length,
                                                       Limits::default()).map_err(format_error)
                                     })?);
                    return Ok(None);
                }
                let builder = m.builder.as_ref().ok_or(Error(mac_abi::BAD_ABI))?;
                if let Some(request) = builder.request().map_err(format_error)?
                {
                    m.fetch = Some(Fetch::new(request.offset, request.length)?);
                    return Ok(None);
                }
                let builder = m.builder.take().ok_or(Error(mac_abi::BAD_ABI))?;
                let index = crate::module_allocation(|| builder.finish().map_err(format_error))?;
                let root = m.root.take().ok_or(Error(mac_abi::BAD_ABI))?;
                let volume =
                    crate::module_allocation(|| Volume::new(m.volume, m.source, index, root))?;
                let output = volume.descriptor()?;
                backend.volumes.push(volume); // Capacity reserved before any I/O.
                Ok(Some(output))
            }
            Self::Read(r) =>
            {
                let result =
                    r.stream
                     .as_mut()
                     .ok_or(Error(mac_abi::BAD_ABI))?
                     .step(&mut backend.sources, r.source, r.offset, &mut r.output,
                           &mut r.written, cancel);
                match result
                {
                    Ok(false) => Ok(None),
                    terminal =>
                    {
                        let open = backend.opens
                                          .iter_mut()
                                          .find(|f| f.volume == r.volume && f.token == r.token)
                                          .ok_or(Error(mac_abi::BAD_ABI))?;
                        // A failed decoder cannot be resumed. All lower reads
                        // have acknowledged before this state is released.
                        if terminal.is_ok()
                        {
                            open.stream = r.stream.take().map(|s| (r.kind, s));
                        }
                        else
                        {
                            open.stream = None;
                            r.stream = None;
                        }
                        terminal.map(|_| Some(core::mem::take(&mut r.output)))
                    }
                }
            }
        }
    }
}
struct Active
{
    ticket:    u64,
    operation: u32,
    volume:    u32,
    token:     u64,
    task:      Task,
    cancel:    bool
}
struct State<S>
{
    backend:   Option<Backend<S>>,
    active:    Vec<Active>,
    running:   bool,
    quiescing: bool
}
pub struct Service<S>
{
    state: Spinlock<State<S>>
}
impl<S: Sources> Default for Service<S>
{
    fn default() -> Self
    {
        Self::new()
    }
}
impl<S: Sources> Service<S>
{
    pub const fn new() -> Self
    {
        Self { state: Spinlock::new(State { backend:   None,
                                            active:    Vec::new(),
                                            running:   false,
                                            quiescing: false }) }
    }

    pub fn initialize(&self, sources: S) -> Result<()>
    {
        let mut state = self.state.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
        if state.backend.is_some() || state.running || !state.active.is_empty() || state.quiescing
        {
            return Err(Error(mac_abi::SERVICE_BUSY));
        }
        state.backend = Some(Backend { sources,
                                       volumes: Vec::new(),
                                       opens: Vec::new() });
        Ok(())
    }

    pub fn request(&self, call: Request<'_>, output: &mut [u8]) -> Result<Completion>
    {
        if call.class.0 != fs::CLASS
           || call.ticket == 0
           || call.input.len() > fs::MAX_REQUEST
           || output.len() > fs::MAX_REPLY
           || call.phase != Phase::Start && !call.input.is_empty()
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        let mut state = self.state.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
        if state.running
        {
            return Err(Error(mac_abi::SERVICE_BUSY));
        }
        let mut active = if call.phase == Phase::Start
        {
            if state.active.len() == MAX_ACTIVE
            {
                return Err(Error(mac_abi::NO_MEMORY));
            }
            if state.active.iter().any(|active| active.ticket == call.ticket)
            {
                return Err(Error(mac_abi::BAD_ARGUMENT));
            }
            crate::module_reserve(&mut state.active, 1)?;
            let op =
                Operation::try_from(call.operation).map_err(|_| Error(mac_abi::NOT_SUPPORTED))?;
            let args = Arguments::decode(call.input).map_err(|_| Error(mac_abi::BAD_ARGUMENT))?;
            validate(op, args)?;
            if state.quiescing && !matches!(op, Operation::Unmount | Operation::Close)
            {
                return Err(Error(mac_abi::SERVICE_BUSY));
            }
            if output.len() < capacity(op, args)
            {
                return Err(Error(mac_abi::BAD_ARGUMENT));
            }
            // Independent reads may progress while another task waits for a
            // backing fork from this same provider. A single open's decoder
            // remains exclusively owned until its task acknowledges completion.
            if state.active.iter().any(|active| {
                                      active.volume == args.volume
                                      && (op == Operation::Unmount
                                          || active.operation == Operation::Mount as u32
                                          || matches!(op, Operation::Read | Operation::Close)
                                             && active.token == args.token)
                                  })
            {
                return Err(Error(mac_abi::SERVICE_BUSY));
            }
            if op == Operation::Mount
            {
                let mounting = state.active
                                    .iter()
                                    .filter(|active| active.operation == Operation::Mount as u32)
                                    .count();
                let backend = state.backend.as_mut().ok_or(Error(mac_abi::NOT_SUPPORTED))?;
                if backend.volumes.len() + mounting >= MAX_MOUNTS
                {
                    return Err(Error(mac_abi::NO_MEMORY));
                }
                // Reserve for every admitted but not yet published mount.
                crate::module_reserve(&mut backend.volumes, mounting + 1)?;
            }
            // No lower invocation during planning. Owned root/catalog/read
            // buffers are prepared before the first callback can pend.
            let task =
                state.backend.as_mut().ok_or(Error(mac_abi::NOT_SUPPORTED))?.start(op, args)?;
            Active { ticket: call.ticket,
                     operation: call.operation,
                     volume: args.volume,
                     token: args.token,
                     task,
                     cancel: false }
        }
        else
        {
            let at = state.active
                          .iter()
                          .position(|active| {
                              active.ticket == call.ticket && active.operation == call.operation
                          })
                          .ok_or(Error(mac_abi::NOT_FOUND))?;
            if call.phase == Phase::Cancel
            {
                state.active[at].cancel = true;
            }
            state.active.swap_remove(at)
        };
        let mut backend = state.backend.take().ok_or(Error(mac_abi::BAD_ABI))?;
        state.running = true;
        drop(state);
        if !matches!(active.task, Task::Ready(_))
        {
            match active.task.advance(&mut backend, active.cancel)
            {
                Ok(None) => (),
                Ok(Some(bytes)) => active.task = Task::Ready(Ok(bytes)),
                Err(error) => active.task = Task::Ready(Err(error))
            }
        }
        let mut state = self.state.lock();
        state.backend = Some(backend);
        state.running = false;
        if let Task::Ready(result) = &active.task
           && result.as_ref().map_or(true, |bytes| bytes.len() <= output.len())
        {
            let Task::Ready(result) = active.task
            else
            {
                unreachable!();
            };
            return result.map(|bytes| {
                             output[..bytes.len()].copy_from_slice(&bytes);
                             Completion::Complete(bytes.len())
                         });
        }
        active.cancel |= state.quiescing;
        state.active.push(active);
        Ok(Completion::Pending)
    }

    pub fn quiesce(&self) -> Result<()>
    {
        let mut state = self.state.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
        state.quiescing = true;
        for active in &mut state.active
        {
            active.cancel = true;
        }
        if state.running
           || !state.active.is_empty()
           || state.backend.as_ref().is_some_and(|b| !b.volumes.is_empty() || !b.opens.is_empty())
        {
            return Err(Error(mac_abi::SERVICE_BUSY));
        }
        // No open decoder, mount or operation remains. Return retained global
        // capacities before the resident attempts the module account barrier.
        let backend = state.backend.take();
        let active = core::mem::take(&mut state.active);
        drop(state);
        drop(backend);
        drop(active);
        Ok(())
    }
}
fn capacity(op: Operation, a: Arguments<'_>) -> usize
{
    match op
    {
        Operation::Mount => fs::MAX_MOUNT_BYTES,
        Operation::Catalog | Operation::Lookup => fs::MAX_METADATA_BYTES,
        Operation::Scan => 4 + fs::MAX_METADATA_BYTES,
        Operation::Open => 16,
        Operation::Read => a.length as usize,
        _ => 0
    }
}
fn validate(op: Operation, a: Arguments<'_>) -> Result<()>
{
    let v = Arguments { volume: a.volume,
                        ..Default::default() };
    let accepted = match op
    {
        Operation::Mount =>
        {
            Arguments { auxiliary: a.auxiliary,
                        payload: a.payload,
                        ..v }
        }
        Operation::Unmount | Operation::Refresh => v,
        Operation::Catalog =>
        {
            Arguments { node: a.node,
                        flags: a.flags,
                        ..v }
        }
        Operation::Lookup =>
        {
            Arguments { parent: a.parent,
                        flags: a.flags,
                        payload: a.payload,
                        ..v }
        }
        Operation::Scan =>
        {
            Arguments { parent: a.parent,
                        offset: a.offset,
                        ..v }
        }
        Operation::Open =>
        {
            Arguments { node: a.node,
                        token: a.token,
                        ..v }
        }
        Operation::Read =>
        {
            Arguments { token: a.token,
                        flags: a.flags,
                        offset: a.offset,
                        length: a.length,
                        ..v }
        }
        Operation::Close =>
        {
            Arguments { token: a.token,
                        ..v }
        }
        Operation::Exports => return Err(Error(mac_abi::NOT_SUPPORTED)),
        _ => return Err(Error(mac_abi::READ_ONLY))
    };
    if a != accepted
       || a.volume == 0
       || matches!(op, Operation::Catalog | Operation::Lookup)
          && a.flags & !fs::INCLUDE_CHILD_COUNT != 0
       || matches!(op, Operation::Open | Operation::Close | Operation::Read) && a.token == 0
       || op == Operation::Read
          && (a.flags > fs::RESOURCE_FORK
              || a.length > fs::MAX_TRANSFER as u64
              || a.offset.checked_add(a.length).is_none())
    {
        return Err(Error(mac_abi::BAD_ARGUMENT));
    }
    Ok(())
}
