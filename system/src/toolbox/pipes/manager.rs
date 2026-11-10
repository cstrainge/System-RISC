//! Byte-transport pipes. A pipe is a bounded ring with one reader endpoint and
//! one writer endpoint; endpoints are generation-tagged handles owned by the
//! process that received them, while the ring and its registered name are
//! System state shared by every process holding an endpoint.
//!
//! No framing, record boundary or message operation exists here. The 512-byte
//! bound is an atomicity rule only.
#![forbid(unsafe_code)]
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use mac_abi::{Status, pipes as wire, *};
use mac_object_table::{domain::ReleasedResource,
                       registry::{Release, Source, Specification},
                       shared::InvocationGate as _};
use spinning_top::Spinlock;

use super::ring::Ring;
use crate::{machine::scheduler::service::{RequestAction, RequestIdentity, RequestResult},
            toolbox::handles::{CapacityPolicy, InvocationGate as Gate, KindCode, OwnerRef, Store}};

/// A named pipe is also a Finder item, so its name uses the HFS item bound.
type Name = (u8, [u8; wire::NAME_BYTES]);

const READER: bool = true;
const WRITER: bool = false;
const MAX_ENDPOINTS: usize = 256;

/// Resident pipe object. Owned by the System, not by any single process: the
/// last endpoint of either kind decides when it may be destroyed.
struct Pipe
{
    ring:           Ring,
    readers:        u32,
    writers:        u32,
    /// Writer handles currently obtained through `connect`. The creator's own
    /// writer is not a connection and is not counted here.
    connections:    u32,
    /// Input pipe carries discipline; output pipes refer to that input identity.
    terminal:       Option<super::terminal::State>,
    terminal_input: u64
}
/// The named-pipe registry. Anonymous pipes never enter this vector, so they
/// have no name, no catalog identity and no object in the folder's namespace:
/// enumeration cannot reveal them, not even through a count.
struct Named
{
    name:    Name,
    pipe:    u64,
    cnid:    u32,
    /// Registration instant in Mac-epoch microseconds, so the item's dates are
    /// stable across enumerations instead of reading the clock per row.
    created: u64,
    /// Maximum concurrent `connect` writers; zero is unlimited. A limit of one
    /// makes the name's writer ownership structural: the second application is
    /// refused at the connection instead of interleaving into the first one's
    /// stream.
    writers: u32
}
/// One registry row as the File Manager sees it. Copied out under the manager
/// lock so a catalog step never holds two locks or borrows registry storage.
pub(crate) struct Row
{
    pub cnid:    u32,
    pub created: u64,
    name:        Name
}
impl Row
{
    pub(crate) fn name(&self) -> &[u8]
    {
        &self.name.1[..self.name.0 as usize]
    }
}
#[derive(Clone, Copy)]
struct Endpoint
{
    pipe:      u64,
    reader:    bool,
    blocking:  bool,
    /// Obtained through `connect`, so its retirement releases a connection
    /// slot of the name's writer limit.
    connected: u64
}
struct Manager
{
    pipes:     Store<Pipe>,
    endpoints: Store<Endpoint>,
    named:     Vec<Named>,
    revision:  u32
}
impl Manager
{
    /// Advance the registry revision. Zero is skipped so an open enumeration
    /// can never mistake a wrapped counter for "no registry".
    fn bump(&mut self)
    {
        self.revision = self.revision.wrapping_add(1);
        if self.revision == 0
        {
            self.revision = 1;
        }
    }
}

static MANAGER: spin::Once<Spinlock<Manager>> = spin::Once::new();
static NEXT_CNID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy)]
pub(crate) enum Registration
{
    Pipe,
    PipeStorage,
    Endpoint,
    EndpointStorage
}
impl Registration
{
    pub(crate) const ALL: [Self; 4] =
        [Self::Pipe, Self::PipeStorage, Self::Endpoint, Self::EndpointStorage];

    pub(crate) fn specification(self) -> Specification<'static>
    {
        let (code, label, capacity) = match self
        {
            Self::Pipe =>
            {
                (*b"PPST",
                 "Named and anonymous pipes",
                 CapacityPolicy::HardBound { slots:  wire::MAX_PIPES,
                                             reason: "Bounded resident pipe population" })
            }
            Self::Endpoint =>
            {
                (*b"PIPE",
                 "Pipe endpoints",
                 CapacityPolicy::HardBound { slots:  MAX_ENDPOINTS,
                                             reason: "Two endpoints per anonymous pipe plus one \
                                                      writer per connection" })
            }
            Self::PipeStorage =>
            {
                (*b"PPBS",
                 "Pipe object storage",
                 CapacityPolicy::HardBound { slots:  1,
                                             reason: "One pipe object backing per owner domain" })
            }
            Self::EndpointStorage =>
            {
                (*b"PEBS",
                 "Pipe endpoint storage",
                 CapacityPolicy::HardBound { slots:  1,
                                             reason: "One endpoint backing per owner domain" })
            }
        };
        Specification { kind: KindCode(code),
                        label,
                        provider: "System Pipes",
                        capacity,
                        source: Source::Resident }
    }
}
impl Release<crate::toolbox::runtime::Toolbox, crate::toolbox::handles::cleanup::Failure>
    for Registration
{
    fn release(&self, _: &mut crate::toolbox::runtime::Toolbox, ticket: ReleasedResource)
               -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        use crate::toolbox::handles::cleanup::Failure;
        if matches!(self, Self::Endpoint)
        {
            return release_endpoint(ticket);
        }
        if matches!(self, Self::Pipe)
        {
            return release_pipe(ticket);
        }
        let manager = match manager()
        {
            Ok(manager) => manager,
            Err(_) => return Err(Failure::context_unavailable(ticket, "Pipes"))
        };
        let mut manager = manager.lock();
        if matches!(self, Self::PipeStorage)
        {
            manager.pipes.release_storage(ticket).map_err(Failure::from)
        }
        else
        {
            manager.endpoints.release_storage(ticket).map_err(Failure::from)
        }
    }
}

fn manager() -> Result<&'static Spinlock<Manager>, Status>
{
    MANAGER.try_call_once(|| {
               let (pipe, pipe_storage, endpoint, endpoint_storage) = crate::toolbox::handles::pipe_kinds()?;
               Ok::<_, Status>(Spinlock::new(Manager { pipes:     Store::owned(pipe, pipe_storage).map_err(status)?,
                                                       endpoints: Store::owned(endpoint, endpoint_storage).map_err(status)?,
                                                       named:     Vec::new(),
                                                       revision:  1 }))
           })
}

fn status(error: mac_object_table::domain::PoolError) -> Status
{
    use mac_object_table::domain::PoolError;
    match error
    {
        PoolError::RegistrationUsesExhausted { .. }
        | PoolError::Exhausted { .. }
        | PoolError::Storage { .. }
        | PoolError::IdentityExhausted
        | PoolError::GrowthRequired { .. }
        | PoolError::GrowthLimit { .. } => NO_MEMORY,
        PoolError::RegistrationUnavailable { .. } | PoolError::Stopping { .. } => SERVICE_BUSY,
        _ => BAD_ARGUMENT
    }
}

fn decode_name(text: &[u8]) -> Result<Name, Status>
{
    if text.is_empty() || text.len() > wire::NAME_BYTES
    {
        return Err(INVALID_NAME);
    }
    // A named pipe is a Finder item, so its name must be representable in the
    // folder listing. Control characters, the path separator and non-ASCII
    // bytes have no item form here and are refused at creation instead of
    // being sanitised on every enumeration.
    if text.iter().any(|byte| !(b' '..=b'~').contains(byte) || *byte == b':')
    {
        return Err(INVALID_NAME);
    }
    let mut bytes = [0u8; wire::NAME_BYTES];
    bytes[..text.len()].copy_from_slice(text);
    Ok((text.len() as u8, bytes))
}

fn same_name(left: &[u8], right: &[u8]) -> bool
{
    left.len() == right.len() && left.iter().zip(right).all(|(a, b)| a.eq_ignore_ascii_case(b))
}

fn named(entry: &Named) -> &[u8]
{
    &entry.name.1[..entry.name.0 as usize]
}

/// Synthetic catalog identity range. HFS-B allocates real CNIDs densely from
/// 16, and classic HFS composes them from catalog node numbers, so this band is
/// reserved for registry entries that are not catalog records.
const VIRTUAL_FIRST: u32 = 0x8000_0000;
const VIRTUAL_END: u32 = 0x9000_0000;

/// A pipe's synthetic identity is never reused, so a stale reference cannot
/// resolve to a different live pipe. Exhaustion is an ordinary refusal.
fn free_cnid() -> Option<u32>
{
    let counter =
        NEXT_CNID.try_update(Ordering::Relaxed, Ordering::Relaxed, |value| value.checked_add(1))
                 .ok()?;
    let cnid = VIRTUAL_FIRST.checked_add(u32::try_from(counter).ok()?)?;
    (cnid < VIRTUAL_END).then_some(cnid)
}

/// Whether `cnid` belongs to the synthetic band. Mutations use this to fail
/// closed on a reserved identity; catalog resolution additionally requires the
/// registry to actually hold the row.
pub(crate) fn virtual_cnid(cnid: u32) -> bool
{
    (VIRTUAL_FIRST..VIRTUAL_END).contains(&cnid)
}

/// Registration instant for a new item, or zero when the clock is unavailable.
/// The date is informational; it never gates registration.
fn registration_time() -> u64
{
    crate::machine::clock::now().ok().and_then(|now| now.mac_microseconds()).unwrap_or(0)
}

/// Registry revision, or zero before the manager exists. An open enumeration
/// carries this value and is stale as soon as it differs.
pub(crate) fn revision() -> u32
{
    let Ok(manager) = manager()
    else
    {
        return 0;
    };
    manager.lock().revision
}

/// Registry row by offset in registration order. The end of the registry is
/// reported as absence, so a caller never needs a separate count.
pub(crate) fn row(index: usize) -> Option<Row>
{
    let manager = manager().ok()?;
    let manager = manager.lock();
    let entry = manager.named.get(index)?;
    Some(Row { cnid:    entry.cnid,
               created: entry.created,
               name:    entry.name })
}

/// Registry row by item name, using the same comparison as `CONNECT`.
pub(crate) fn find(text: &[u8]) -> Option<Row>
{
    let manager = manager().ok()?;
    let manager = manager.lock();
    let entry = manager.named.iter().find(|entry| same_name(named(entry), text))?;
    Some(Row { cnid:    entry.cnid,
               created: entry.created,
               name:    entry.name })
}

/// Registry row by synthetic identity.
pub(crate) fn row_of(cnid: u32) -> Option<Row>
{
    let manager = manager().ok()?;
    let manager = manager.lock();
    let entry = manager.named.iter().find(|entry| entry.cnid == cnid)?;
    Some(Row { cnid:    entry.cnid,
               created: entry.created,
               name:    entry.name })
}

/// Validate a delivered request, perform it, and park only when the contract
/// says the caller must wait. The trap boundary already validated the caller's
/// buffer; `input`/`output` are that same span in the operation's direction.
pub(crate) fn request(call: &mut wire::Call, input: &[u8], output: &mut [u8],
                      identity: RequestIdentity, suspension: Result<(), Status>)
                      -> RequestResult
{
    call.transferred = 0;
    call.reader = 0;
    call.writer = 0;
    let result =
        crate::context::without_recovery(|| perform(call, input, output, identity, suspension));
    match result
    {
        Ok(action) =>
        {
            RequestResult { status: OK,
                            action }
        }
        Err(cause) =>
        {
            crate::machine::scheduler::pipe::cancel_current();
            RequestResult { status: cause,
                            action: RequestAction::Return }
        }
    }
}

fn perform(call: &mut wire::Call, input: &[u8], output: &mut [u8], identity: RequestIdentity,
           suspension: Result<(), Status>)
           -> Result<RequestAction, Status>
{
    if call.reserved != [0; 1]
       || call.terminal.reserved != 0
       || call.nonblocking > 1
       || (!matches!(call.operation, wire::READ_WAIT | wire::TERMINAL_READ_WAIT)
           && call.wait_nanoseconds != 0)
    {
        return Err(BAD_ARGUMENT);
    }
    if !matches!(call.operation, wire::TERMINAL_ATTACH) && call.terminal_outputs != [0; 2]
    {
        return Err(BAD_ARGUMENT);
    }
    let owner = crate::context::current_owner()?;
    // A resumed call must be the same request; a changed one is refused before
    // it can consume a different handle than the one that parked.
    let turn = crate::machine::scheduler::pipe::begin(identity, call.handle, call.operation,
        matches!(call.operation, wire::READ_WAIT | wire::TERMINAL_READ_WAIT).then_some(call.wait_nanoseconds))?;
    let resumed = turn.resumed;
    match call.operation
    {
        wire::CREATE | wire::CREATE_NAMED =>
        {
            if resumed || call.handle != 0 || !output.is_empty() || !input.is_empty()
            {
                return Err(BAD_ARGUMENT);
            }
            let name = if call.operation == wire::CREATE_NAMED
            {
                Some(decode_name(call.name
                                     .get(..call.name_length as usize)
                                     .ok_or(BAD_ARGUMENT)?)?)
            }
            else
            {
                // An anonymous pipe has no name, so it has no connections and
                // no writer limit to state.
                if call.name_length != 0 || call.writers != 0
                {
                    return Err(BAD_ARGUMENT);
                }
                None
            };
            create(call, &owner, name)
        }
        wire::CONNECT =>
        {
            if resumed
               || call.handle != 0
               || call.writers != 0
               || !output.is_empty()
               || !input.is_empty()
            {
                return Err(BAD_ARGUMENT);
            }
            let name =
                decode_name(call.name.get(..call.name_length as usize).ok_or(BAD_ARGUMENT)?)?;
            connect(call, &owner, name)
        }
        wire::READ | wire::READ_WAIT | wire::TERMINAL_READ_WAIT =>
        {
            if call.handle == 0 || call.name_length != 0 || call.writers != 0 || !input.is_empty()
            {
                return Err(BAD_ARGUMENT);
            }
            read(call, &owner, output, identity, suspension, turn)
        }
        wire::WRITE =>
        {
            if call.handle == 0 || call.name_length != 0 || call.writers != 0 || !output.is_empty()
            {
                return Err(BAD_ARGUMENT);
            }
            write(call, &owner, input, identity, suspension, turn)
        }
        wire::SET_BLOCKING =>
        {
            if resumed
               || call.handle == 0
               || call.writers != 0
               || !output.is_empty()
               || !input.is_empty()
            {
                return Err(BAD_ARGUMENT);
            }
            set_blocking(call, &owner)
        }
        wire::TERMINAL_ATTACH
        | wire::TERMINAL_QUERY
        | wire::TERMINAL_MODE
        | wire::TERMINAL_RESIZE =>
        {
            if resumed
               || call.handle == 0
               || call.name_length != 0
               || call.writers != 0
               || !input.is_empty()
               || !output.is_empty()
            {
                return Err(BAD_ARGUMENT);
            }
            terminal_control(call, &owner)
        }
        wire::TERMINAL_INTERRUPT =>
        {
            if resumed
               || !input.is_empty()
               || !output.is_empty()
               || call.writers != 0
               || call.name_length != 0
            {
                return Err(BAD_ARGUMENT);
            }
            terminal_interrupt(call, &owner)
        }
        wire::TERMINAL_INPUT =>
        {
            if call.handle == 0
               || call.name_length != 0
               || call.writers != 0
               || !output.is_empty()
               || input.is_empty()
            {
                return Err(BAD_ARGUMENT);
            }
            terminal_input(call, &owner, input, identity, suspension, turn)
        }
        wire::DUPLICATE =>
        {
            if resumed
               || call.handle == 0
               || call.writers != 0
               || !output.is_empty()
               || !input.is_empty()
            {
                return Err(BAD_ARGUMENT);
            }
            duplicate(call, &owner)
        }
        wire::CLOSE =>
        {
            if resumed
               || call.handle == 0
               || call.writers != 0
               || !output.is_empty()
               || !input.is_empty()
            {
                return Err(BAD_ARGUMENT);
            }
            close(call, &owner)
        }
        _ => Err(BAD_ARGUMENT)
    }
}

fn create(call: &mut wire::Call, owner: &OwnerRef, name: Option<Name>)
          -> Result<RequestAction, Status>
{
    let capacity = call.capacity as usize;
    if !(wire::MIN_CAPACITY..=wire::MAX_CAPACITY).contains(&capacity)
    {
        return Err(BAD_ARGUMENT);
    }
    let Some(ring) = Ring::with_capacity(capacity)
    else
    {
        return Err(NO_MEMORY);
    };
    let system = crate::toolbox::handles::system_owner()?;
    let manager = manager()?;
    let mut manager = manager.lock();
    let mut cnid = None;
    let mut limit = 0;
    if let Some(name) = &name
    {
        for entry in &manager.named
        {
            if same_name(named(entry), &name.1[..name.0 as usize])
            {
                return Err(ALREADY_EXISTS);
            }
        }
        limit = u32::try_from(call.writers).map_err(|_| BAD_ARGUMENT)?;
        // Preflight the registry slot before any pipe or endpoint exists, so a
        // refused insertion cannot leave a half-registered name. The identity
        // is taken before the objects so exhaustion is refused before them
        // too; a failed creation burns an identity rather than reusing one.
        Gate::with_system_allocation(|| manager.named.try_reserve(1)).map_err(|_| NO_MEMORY)?;
        cnid = Some(free_cnid().ok_or(NO_MEMORY)?);
    }
    let pipe = Gate::with_system_allocation(|| {
                   manager.pipes.insert(&system,
                                        Pipe { ring,
                                               readers: 1,
                                               writers: 1,
                                               connections: 0,
                                               terminal: None,
                                               terminal_input: 0 })
               }).map_err(status)?;
    let reader = match manager.endpoints.insert(owner,
                                                Endpoint { pipe,
                                                           reader: READER,
                                                           blocking: call.nonblocking == 0,
                                                           connected: 0 })
    {
        Ok(id) => id,
        Err(error) =>
        {
            let _ = manager.pipes.remove(pipe);
            return Err(status(error));
        }
    };
    let writer = match manager.endpoints.insert(owner,
                                                Endpoint { pipe,
                                                           reader: WRITER,
                                                           blocking: call.nonblocking == 0,
                                                           connected: 0 })
    {
        Ok(id) => id,
        Err(error) =>
        {
            let _ = manager.endpoints.remove(reader);
            let _ = manager.pipes.remove(pipe);
            return Err(status(error));
        }
    };
    if let (Some(name), Some(cnid)) = (name, cnid)
    {
        manager.named.push(Named { name,
                                   pipe,
                                   cnid,
                                   created: registration_time(),
                                   writers: limit });
        manager.bump();
    }
    call.reader = reader;
    call.writer = writer;
    Ok(RequestAction::Return)
}

/// Add a writer end for a registered name. The name's writer limit bounds the
/// concurrent connections, not the total writer count: the creator already
/// holds a writer handle that is not a connection. A full name is refused with
/// `AccessDenied` and consumes nothing.
fn connect(call: &mut wire::Call, owner: &OwnerRef, name: Name) -> Result<RequestAction, Status>
{
    call.writer = connect_named(owner,
                                name.1.get(..name.0 as usize).ok_or(BAD_ARGUMENT)?,
                                call.nonblocking == 0)?;
    Ok(RequestAction::Return)
}

/// Take one connection slot on `name` and return its writer handle. Shared by
/// the `CONNECT` operation and by the File Manager's open-as-connect, so both
/// apply the same writer limit and the same endpoint ownership.
fn connect_named(owner: &OwnerRef, name: &[u8], blocking: bool) -> Result<u64, Status>
{
    let manager = manager()?;
    let mut manager = manager.lock();
    let Some((pipe, limit)) = manager.named
                                     .iter()
                                     .find(|entry| same_name(named(entry), name))
                                     .map(|entry| (entry.pipe, entry.writers))
    else
    {
        return Err(NOT_FOUND);
    };
    let Some(state) = manager.pipes.get(pipe)
    else
    {
        return Err(NOT_FOUND);
    };
    if limit != 0 && state.connections >= limit
    {
        return Err(ACCESS_DENIED);
    }
    let writers = state.writers.checked_add(1).ok_or(BAD_ARGUMENT)?;
    let connections = state.connections.checked_add(1).ok_or(BAD_ARGUMENT)?;
    let writer = manager.endpoints
                        .insert(owner,
                                Endpoint { pipe,
                                           reader: WRITER,
                                           blocking,
                                           connected: 0 })
                        .map_err(status)?;
    // The original handle supplies a never-reused connection group identity.
    // Aliases retain it; only the last alias releases the named connection.
    if let Some(endpoint) = manager.endpoints.get_mut(writer)
    {
        endpoint.connected = writer;
    }
    let Some(state) = manager.pipes.get_mut(pipe)
    else
    {
        let _ = manager.endpoints.remove(writer);
        return Err(NOT_FOUND);
    };
    state.writers = writers;
    state.connections = connections;
    Ok(writer)
}

/// `connect` for an item the File Manager resolved by identity. The registry is
/// consulted again here, so a pipe destroyed between a listing and the open is
/// refused rather than joined to a name that no longer exists.
pub(crate) fn connect_item(owner: &OwnerRef, cnid: u32, blocking: bool) -> Result<u64, Status>
{
    let manager = manager()?;
    let name = {
        let manager = manager.lock();
        manager.named
               .iter()
               .find(|entry| entry.cnid == cnid)
               .map(|entry| named(entry).to_vec())
               .ok_or(NOT_FOUND)?
    };
    connect_named(owner, &name, blocking)
}

fn read(call: &mut wire::Call, owner: &OwnerRef, output: &mut [u8], identity: RequestIdentity,
        suspension: Result<(), Status>, turn: crate::machine::scheduler::pipe::Turn)
        -> Result<RequestAction, Status>
{
    if output.is_empty()
    {
        return Err(BAD_ARGUMENT);
    }
    let manager = manager()?;
    let mut manager = manager.lock();
    let endpoint = endpoint(&manager, call.handle, owner, READER)?;
    let (status, wake) = {
        let Some(pipe) = manager.pipes.get_mut(endpoint.pipe)
        else
        {
            return Err(NOT_FOUND);
        };
        if call.operation == wire::TERMINAL_READ_WAIT
        {
            let state = pipe.terminal.as_ref().ok_or(NOT_SUPPORTED)?;
            let changed = call.terminal.revision != state.info.revision;
            call.terminal = state.info;
            if changed
            {
                return Ok(RequestAction::Return);
            }
        }
        if let Some(count) =
            pipe.terminal.as_mut().and_then(|state| state.read(output, pipe.writers == 0))
        {
            call.transferred = count;
            (OK, Some(endpoint.pipe))
        }
        else if pipe.ring.available() > 0
        {
            let count = pipe.ring.read_into(output);
            call.transferred = count;
            (OK, Some(endpoint.pipe))
        }
        else if pipe.writers == 0
        {
            // End of stream: the only zero-byte read success.
            call.transferred = 0;
            (OK, None)
        }
        else if (matches!(call.operation, wire::READ_WAIT | wire::TERMINAL_READ_WAIT)
                   && turn.expired())
                  || (call.operation == wire::READ && !endpoint.blocking)
        {
            (WOULD_BLOCK, None)
        }
        else
        {
            crate::machine::scheduler::pipe::park(endpoint.pipe, call.handle, call.operation,
                                                  identity, suspension, turn)?;
            return Ok(RequestAction::Replay);
        }
    };
    let related = manager.pipes.get(endpoint.pipe).map_or(0, |pipe| pipe.terminal_input);
    drop(manager);
    if let Some(pipe) = wake
    {
        crate::machine::scheduler::pipe::wake(pipe);
        if related != 0 && related != pipe
        {
            crate::machine::scheduler::pipe::wake(related);
        }
    }
    if status == OK { Ok(RequestAction::Return) } else { Err(status) }
}

fn write(call: &mut wire::Call, owner: &OwnerRef, input: &[u8], identity: RequestIdentity,
         suspension: Result<(), Status>, turn: crate::machine::scheduler::pipe::Turn)
         -> Result<RequestAction, Status>
{
    if input.is_empty()
    {
        return Err(BAD_ARGUMENT);
    }
    let manager = manager()?;
    let mut manager = manager.lock();
    let endpoint = endpoint(&manager, call.handle, owner, WRITER)?;
    let (status, wake) = {
        let Some(pipe) = manager.pipes.get_mut(endpoint.pipe)
        else
        {
            return Err(NOT_FOUND);
        };
        if pipe.readers == 0
        {
            pipe.ring.clear();
            (BROKEN_PIPE, None)
        }
        else
        {
            let space = pipe.ring.space();
            // An atomic write is all-or-nothing: it waits for the whole span.
            // A larger write moves at least one byte and has no atomicity.
            let count = if input.len() <= wire::ATOMIC_WRITE
            {
                if space >= input.len() { input.len() } else { 0 }
            }
            else
            {
                space.min(input.len())
            };
            if count > 0
            {
                call.transferred = pipe.ring.write_from(input);
                (OK, Some(endpoint.pipe))
            }
            else if !endpoint.blocking
            {
                (WOULD_BLOCK, None)
            }
            else
            {
                crate::machine::scheduler::pipe::park(endpoint.pipe, call.handle, call.operation,
                                                      identity, suspension, turn)?;
                return Ok(RequestAction::Replay);
            }
        }
    };
    drop(manager);
    if let Some(pipe) = wake
    {
        crate::machine::scheduler::pipe::wake(pipe);
    }
    if status == OK { Ok(RequestAction::Return) } else { Err(status) }
}

/// Each alias owns one endpoint count. Allocation and count overflow are
/// preflighted before publication, so refusal cannot change EOF semantics.
fn duplicate(call: &mut wire::Call, owner: &OwnerRef) -> Result<RequestAction, Status>
{
    let mut manager = manager()?.lock();
    let mut endpoint = own_endpoint(&manager, call.handle, owner)?;
    // A connected alias retains the existing connection group; it does not
    // consume a second named-writer slot.
    let pipe = manager.pipes.get(endpoint.pipe).ok_or(NOT_FOUND)?;
    let count = if endpoint.reader { pipe.readers } else { pipe.writers };
    let next = count.checked_add(1).ok_or(NO_MEMORY)?;
    endpoint.blocking = call.nonblocking == 0;
    let id = manager.endpoints.insert(owner, endpoint).map_err(status)?;
    let Some(pipe) = manager.pipes.get_mut(endpoint.pipe)
    else
    {
        let _ = manager.endpoints.remove(id);
        return Err(NOT_FOUND);
    };
    if endpoint.reader
    {
        pipe.readers = next;
        call.reader = id;
    }
    else
    {
        pipe.writers = next;
        call.writer = id;
    }
    Ok(RequestAction::Return)
}

fn set_blocking(call: &mut wire::Call, owner: &OwnerRef) -> Result<RequestAction, Status>
{
    let manager = manager()?;
    let mut manager = manager.lock();
    own_endpoint(&manager, call.handle, owner)?;
    let Some(entry) = manager.endpoints.get_mut(call.handle)
    else
    {
        return Err(BAD_ARGUMENT);
    };
    entry.blocking = call.nonblocking == 0;
    Ok(RequestAction::Return)
}

fn close(call: &mut wire::Call, owner: &OwnerRef) -> Result<RequestAction, Status>
{
    let manager = manager()?;
    let mut manager = manager.lock();
    let endpoint = own_endpoint(&manager, call.handle, owner)?;
    let Some(removed) = manager.endpoints.remove(call.handle)
    else
    {
        return Err(BAD_ARGUMENT);
    };
    drop(removed);
    let related = manager.pipes.get(endpoint.pipe).map_or(0, |pipe| pipe.terminal_input);
    let echo = retire_endpoint(&mut manager, endpoint, owner.owner().incarnation());
    drop(manager);
    // Readers see end of stream, writers see broken pipe, and a destroyed pipe
    // also releases anyone still parked on it.
    crate::machine::scheduler::pipe::wake(endpoint.pipe);
    if related != 0 && related != endpoint.pipe
    {
        crate::machine::scheduler::pipe::wake(related);
    }
    if echo != 0
    {
        crate::machine::scheduler::pipe::wake(echo);
    }
    Ok(RequestAction::Return)
}

/// Apply one endpoint's departure to its pipe. A pipe with no endpoints left is
/// destroyed here; the caller wakes the pipe after the manager lock ends.
fn retire_endpoint(manager: &mut Manager, endpoint: Endpoint, owner: u64) -> u64
{
    let last_reader =
        endpoint.reader
        && !manager.endpoints.iter().any(|(id, other)| {
                                        other.reader && other.pipe == endpoint.pipe
            && manager.endpoints.owner(id).is_some_and(|domain| domain.incarnation() == owner)
                                    });
    let last_connection =
        endpoint.connected != 0
        && !manager.endpoints.iter().any(|(_, other)| other.connected == endpoint.connected);
    let Some(pipe) = manager.pipes.get_mut(endpoint.pipe)
    else
    {
        return 0;
    };
    if last_reader && let Some(state) = &mut pipe.terminal
    {
        state.reset(owner);
    }
    let mut echo_endpoint = 0;
    if endpoint.reader
    {
        pipe.readers = pipe.readers.saturating_sub(1);
        if pipe.readers == 0
        {
            pipe.ring.clear();
            if let Some(state) = &mut pipe.terminal
            {
                state.cancel_input();
                echo_endpoint = core::mem::take(&mut state.echo_endpoint);
            }
        }
    }
    else
    {
        pipe.writers = pipe.writers.saturating_sub(1);
        if last_connection
        {
            pipe.connections = pipe.connections.saturating_sub(1);
        }
    }
    if pipe.readers == 0 && pipe.writers == 0
    {
        let _ = manager.pipes.remove(endpoint.pipe);
        let before = manager.named.len();
        manager.named.retain(|entry| entry.pipe != endpoint.pipe);
        if manager.named.len() != before
        {
            manager.bump();
        }
    }
    if echo_endpoint != 0
       && let Some(removed) = manager.endpoints.remove(echo_endpoint)
    {
        let echo = *removed;
        drop(removed);
        retire_endpoint(manager, echo, 0);
        return echo.pipe;
    }
    0
}

/// Validate that `handle` is a live endpoint of `owner` with the required kind.
fn endpoint(manager: &Manager, handle: u64, owner: &OwnerRef, reader: bool)
            -> Result<Endpoint, Status>
{
    let endpoint = own_endpoint(manager, handle, owner)?;
    if endpoint.reader != reader
    {
        return Err(BAD_ARGUMENT);
    }
    Ok(endpoint)
}

fn own_endpoint(manager: &Manager, handle: u64, owner: &OwnerRef) -> Result<Endpoint, Status>
{
    let Some(domain) = manager.endpoints.owner(handle)
    else
    {
        return Err(BAD_ARGUMENT);
    };
    if domain.incarnation() != owner.owner().incarnation()
    {
        return Err(ACCESS_DENIED);
    }
    manager.endpoints.get(handle).copied().ok_or(BAD_ARGUMENT)
}

/// Registered cleanup entry. The handle table has already invalidated `ticket`;
/// this only releases the endpoint and applies its departure.
fn release_endpoint(ticket: ReleasedResource)
                    -> Result<(), crate::toolbox::handles::cleanup::Failure>
{
    let Ok(manager) = manager()
    else
    {
        return Err(crate::toolbox::handles::cleanup::Failure::context_unavailable(ticket,
                                                                                  "Pipes"));
    };
    let mut manager = manager.lock();
    let owner = ticket.key().owner().incarnation();
    let removed = match manager.endpoints.release(ticket)
    {
        Ok(removed) => removed,
        Err(error) => return Err(error.into())
    };
    let endpoint = *removed;
    drop(removed);
    let related = manager.pipes.get(endpoint.pipe).map_or(0, |pipe| pipe.terminal_input);
    let echo = retire_endpoint(&mut manager, endpoint, owner);
    drop(manager);
    crate::machine::scheduler::pipe::wake(endpoint.pipe);
    if related != 0 && related != endpoint.pipe
    {
        crate::machine::scheduler::pipe::wake(related);
    }
    if echo != 0
    {
        crate::machine::scheduler::pipe::wake(echo);
    }
    Ok(())
}

fn release_pipe(ticket: ReleasedResource) -> Result<(), crate::toolbox::handles::cleanup::Failure>
{
    use crate::toolbox::handles::cleanup::Failure;
    let manager = match manager()
    {
        Ok(manager) => manager,
        Err(_) => return Err(Failure::context_unavailable(ticket, "Pipes"))
    };
    let mut manager = manager.lock();
    if let Some(pipe) = manager.pipes.releasing(&ticket)
    {
        if pipe.readers != 0 || pipe.writers != 0
        {
            return Err(Failure::pipe_endpoints_pending(ticket, pipe.readers, pipe.writers));
        }
    }
    manager.pipes.release(ticket).map(drop).map_err(Failure::from)
}

/// Move the three offered ends as part of launch publication. The caller's
/// invocation gate prevents the new thread or either owner walk from running
/// before the old aliases are invalidated.
pub(crate) fn attach_for_launch(sender: &OwnerRef, child: &OwnerRef,
                                allocation: crate::machine::scheduler::accounting::AllocationAuthority,
                                offered: [u64; 3],
                                start: impl FnOnce([u64; 3]) -> Result<(), Status>)
                                -> Result<(), Status>
{
    if !sender.accepting()
       || !child.accepting()
       || sender.owner() == child.owner()
       || allocation.owner() != child.owner()
    {
        return Err(ACCESS_DENIED);
    }
    if offered[0] == offered[1] || offered[0] == offered[2] || offered[1] == offered[2]
    {
        return Err(BAD_ARGUMENT);
    }
    let mut manager = manager()?.lock();
    let endpoints = [own_endpoint(&manager, offered[0], sender)?,
                     own_endpoint(&manager, offered[1], sender)?,
                     own_endpoint(&manager, offered[2], sender)?];
    if !endpoints[0].reader || endpoints[1].reader || endpoints[2].reader
    {
        return Err(BAD_ARGUMENT);
    }
    let mut attached = [0; 3];
    let result = (|| {
        for (index, endpoint) in endpoints.into_iter().enumerate()
        {
            attached[index] = crate::allocator::with_target(crate::allocator::Target::Owner {
                account: allocation.clone(), class: mac_kernel_model::policy::MemoryClass::Kernel
            }, || manager.endpoints.insert(child, endpoint)).map_err(status)?;
        }
        start(attached)
    })();
    // Alias publication never changes the ring's reader/writer accounting.
    // Only the surviving set of aliases owns the original endpoint counts.
    if let Err(error) = result
    {
        for handle in attached.into_iter().filter(|&handle| handle != 0)
        {
            let _ = manager.endpoints.remove(handle);
        }
        return Err(error);
    }
    for handle in offered
    {
        let _ = manager.endpoints.remove(handle);
    }
    drop(manager);
    for endpoint in endpoints
    {
        crate::machine::scheduler::pipe::wake(endpoint.pipe);
    }
    Ok(())
}

/// Non-consuming, owner-checked readiness. The returned ring identity is only
/// retained by the scheduler; callers continue to use their original endpoint.
pub(crate) fn readiness(owner: &OwnerRef, source: mac_abi::readiness::Source)
                        -> Result<(u64, u32), Status>
{
    use mac_abi::readiness as r;
    let manager = manager()?.lock();
    let endpoint = match own_endpoint(&manager, source.id, owner)
    {
        Ok(e) => e,
        Err(BAD_ARGUMENT) => return Ok((0, r::INVALID)),
        Err(e) => return Err(e)
    };
    if endpoint.reader != (source.kind == r::PIPE_READ)
    {
        return Err(BAD_ARGUMENT);
    }
    let pipe = manager.pipes.get(endpoint.pipe).ok_or(NOT_FOUND)?;
    let flags =
        if endpoint.reader
        {
            if source.minimum != 0
            {
                return Err(BAD_ARGUMENT);
            }
            if pipe.writers == 0
            {
                r::READY | r::CLOSED
            }
            else if pipe.ring.available() != 0
                      || pipe.terminal.as_ref().is_some_and(|state| state.readable(false))
            {
                r::READY
            }
            else
            {
                0
            }
        }
        else
        {
            if source.minimum == 0 || source.minimum > wire::ATOMIC_WRITE as u64
            {
                return Err(BAD_ARGUMENT);
            }
            if pipe.readers == 0
            {
                r::READY | r::CLOSED
            }
            else if pipe.terminal.as_ref().is_some_and(|state| {
                                                state.info.flags & wire::TERMINAL_ECHO != 0
                                                && manager.pipes
                                                          .get(state.output)
                                                          .is_none_or(|output| output.readers == 0)
                                            })
            {
                r::READY | r::CLOSED
            }
            else if pipe.terminal.as_ref().map_or_else(
                                                         || {
                                                             pipe.ring.space()
                                                             >= source.minimum as usize
                                                         },
                                                         |state| {
                                                             state.writable(&pipe.ring,
                manager.pipes.get(state.output).filter(|out| out.readers != 0)
                    .map_or(0, |out| out.ring.space()), source.minimum as usize)
                                                         }
            )
            {
                r::READY
            }
            else
            {
                0
            }
        };
    Ok((endpoint.pipe, flags))
}

/// All three frontend endpoints are authenticated before any association is
/// published. No named pipe or already-associated stream can be repurposed.
fn terminal_control(call: &mut wire::Call, owner: &OwnerRef) -> Result<RequestAction, Status>
{
    let mut manager = manager()?.lock();
    let endpoint = own_endpoint(&manager, call.handle, owner)?;
    if call.operation == wire::TERMINAL_ATTACH
    {
        if endpoint.reader
        {
            return Err(BAD_ARGUMENT);
        }
        let out = endpoint_for_terminal(&manager, call.terminal_outputs[0], owner)?;
        let err = endpoint_for_terminal(&manager, call.terminal_outputs[1], owner)?;
        let ids = [endpoint.pipe, out, err];
        if ids[0] == ids[1] || ids[0] == ids[2] || ids[1] == ids[2]
        {
            return Err(BAD_ARGUMENT);
        }
        for id in ids
        {
            let pipe = manager.pipes.get(id).ok_or(NOT_FOUND)?;
            if pipe.terminal_input != 0 || manager.named.iter().any(|entry| entry.pipe == id)
            {
                return Err(BAD_ARGUMENT);
            }
        }
        let mut state = super::terminal::State::new(call.terminal, out)?;
        let writers =
            manager.pipes.get(out).ok_or(NOT_FOUND)?.writers.checked_add(1).ok_or(NO_MEMORY)?;
        let system = crate::toolbox::handles::system_owner()?;
        state.echo_endpoint = Gate::with_system_allocation(|| {
                                  manager.endpoints.insert(&system,
                                                           Endpoint { pipe:      out,
                                                                      reader:    WRITER,
                                                                      blocking:  false,
                                                                      connected: 0 })
                              }).map_err(status)?;
        if let Some(pipe) = manager.pipes.get_mut(out)
        {
            pipe.writers = writers;
        }
        // Echo is a genuine System-owned producer, so a child's early stdout
        // close does not manufacture EOF while input discipline still echoes.
        // The manager lock pins all preflighted identities until publication.
        for id in ids
        {
            if let Some(pipe) = manager.pipes.get_mut(id)
            {
                pipe.terminal_input = endpoint.pipe;
            }
        }
        if let Some(pipe) = manager.pipes.get_mut(endpoint.pipe)
        {
            pipe.terminal = Some(state);
        }
    }
    let source = manager.pipes.get(endpoint.pipe).ok_or(NOT_FOUND)?.terminal_input;
    if source == 0
    {
        return Err(NOT_SUPPORTED);
    }
    let state = manager.pipes
                       .get_mut(source)
                       .and_then(|pipe| pipe.terminal.as_mut())
                       .ok_or(NOT_SUPPORTED)?;
    let previous = state.info.revision;
    match call.operation
    {
        wire::TERMINAL_MODE =>
        {
            if endpoint.pipe != source || !endpoint.reader
            {
                return Err(ACCESS_DENIED);
            }
            Gate::with_system_allocation(|| {
                state.set_mode(call.terminal.flags, owner.owner().incarnation())
            })?;
        }
        wire::TERMINAL_RESIZE =>
        {
            if endpoint.pipe != source || endpoint.reader
            {
                return Err(ACCESS_DENIED);
            }
            state.resize(call.terminal)?;
        }
        _ => ()
    }
    call.terminal = state.info;
    let changed = previous != state.info.revision || call.operation == wire::TERMINAL_ATTACH;
    drop(manager);
    if changed
    {
        crate::machine::scheduler::pipe::wake(source);
    }
    Ok(RequestAction::Return)
}
fn endpoint_for_terminal(manager: &Manager, handle: u64, owner: &OwnerRef) -> Result<u64, Status>
{
    Ok(endpoint(manager, handle, owner, READER)?.pipe)
}

fn terminal_input(call: &mut wire::Call, owner: &OwnerRef, input: &[u8],
                  identity: RequestIdentity, suspension: Result<(), Status>,
                  turn: crate::machine::scheduler::pipe::Turn)
                  -> Result<RequestAction, Status>
{
    let mut manager = manager()?.lock();
    let endpoint = endpoint(&manager, call.handle, owner, WRITER)?;
    let pipe = manager.pipes.get(endpoint.pipe).ok_or(NOT_FOUND)?;
    if pipe.readers == 0
    {
        return Err(BROKEN_PIPE);
    }
    let state = pipe.terminal.as_ref().ok_or(NOT_SUPPORTED)?;
    let echo = state.output;
    if state.info.flags & wire::TERMINAL_ECHO != 0
       && manager.pipes.get(echo).is_none_or(|output| output.readers == 0)
    {
        return Err(BROKEN_PIPE);
    }
    if state.info.flags == wire::TERMINAL_RAW && state.writable(&pipe.ring, 0, 1)
    {
        let pipe = manager.pipes.get_mut(endpoint.pipe).ok_or(NOT_FOUND)?;
        call.transferred = pipe.ring.write_from(&input[..input.len().min(wire::ATOMIC_WRITE)]);
        drop(manager);
        crate::machine::scheduler::pipe::wake(endpoint.pipe);
        return Ok(RequestAction::Return);
    }
    let mut refusal = None;
    // One invocation does bounded work even if a caller supplies a huge span.
    for &byte in input.iter().take(wire::ATOMIC_WRITE)
    {
        let interrupt =
            byte == 3
            && manager.pipes
                      .get(endpoint.pipe)
                      .and_then(|pipe| pipe.terminal.as_ref())
                      .is_some_and(|state| state.info.flags & wire::TERMINAL_INTERRUPTS != 0);
        if interrupt
        {
            // A previous byte has already committed. Return that progress;
            // the next request admits the interrupt before consuming Ctrl+C.
            if call.transferred != 0
            {
                break;
            }
            let targets = InterruptTargets::new(&manager, endpoint.pipe, owner);
            drop(manager);
            crate::toolbox::processes::termination::interrupt(&targets)?;
            let mut manager = self::manager()?.lock();
            if let Some(pipe) = manager.pipes.get_mut(endpoint.pipe)
            {
                pipe.ring.clear();
                if let Some(state) = &mut pipe.terminal
                {
                    state.cancel_input();
                }
            }
            call.transferred = 1;
            drop(manager);
            crate::machine::scheduler::pipe::wake(endpoint.pipe);
            return Ok(RequestAction::Return);
        }
        let space = manager.pipes
                           .get(echo)
                           .filter(|pipe| pipe.readers != 0)
                           .map_or(0, |pipe| pipe.ring.space());
        let pipe = manager.pipes.get_mut(endpoint.pipe).ok_or(NOT_FOUND)?;
        let state = pipe.terminal.as_mut().ok_or(NOT_SUPPORTED)?;
        let result = Gate::with_system_allocation(|| state.input(byte, &mut pipe.ring, space));
        match result
        {
            Ok((bytes, length)) =>
            {
                if length != 0
                   && let Some(output) = manager.pipes.get_mut(echo)
                {
                    output.ring.write_from(&bytes[..length]);
                }
                call.transferred += 1;
            }
            Err(cause) =>
            {
                refusal = Some(cause);
                break;
            }
        }
    }
    if call.transferred == 0
    {
        let cause = refusal.unwrap_or(WOULD_BLOCK);
        if cause == WOULD_BLOCK && endpoint.blocking
        {
            crate::machine::scheduler::pipe::park(endpoint.pipe, call.handle, call.operation,
                                                  identity, suspension, turn)?;
            return Ok(RequestAction::Replay);
        }
        return Err(cause);
    }
    drop(manager);
    crate::machine::scheduler::pipe::wake(endpoint.pipe);
    crate::machine::scheduler::pipe::wake(echo);
    Ok(RequestAction::Return)
}

/// Authority captured only from an authenticated terminal frontend write.
/// Process IDs here are the owners of live input endpoints of this exact pipe.
pub(crate) struct InterruptTargets
{
    host:   u64,
    owners: [u64; MAX_ENDPOINTS],
    count:  usize
}
impl InterruptTargets
{
    fn new(manager: &Manager, pipe: u64, owner: &OwnerRef) -> Self
    {
        let host = owner.owner().incarnation();
        let mut targets = Self { host,
                                 owners: [0; MAX_ENDPOINTS],
                                 count: 0 };
        for (id, endpoint) in manager.endpoints.iter()
        {
            if endpoint.pipe != pipe || !endpoint.reader
            {
                continue;
            }
            if let Some(domain) = manager.endpoints.owner(id)
            {
                let id = domain.incarnation();
                if id != host && !targets.owners[..targets.count].contains(&id)
                {
                    // Endpoint population is bounded by this same limit.
                    targets.owners[targets.count] = id;
                    targets.count += 1;
                }
            }
        }
        targets
    }

    pub(crate) fn host(&self) -> u64
    {
        self.host
    }

    pub(crate) fn owners(&self) -> &[u64]
    {
        &self.owners[..self.count]
    }
}

fn terminal_interrupt(call: &mut wire::Call, owner: &OwnerRef) -> Result<RequestAction, Status>
{
    let manager = manager()?.lock();
    let endpoint = endpoint(&manager, call.handle, owner, WRITER)?;
    let state = manager.pipes
                       .get(endpoint.pipe)
                       .and_then(|pipe| pipe.terminal.as_ref())
                       .ok_or(NOT_SUPPORTED)?;
    if state.info.flags & wire::TERMINAL_INTERRUPTS == 0
    {
        return Err(NOT_SUPPORTED);
    }
    let targets = InterruptTargets::new(&manager, endpoint.pipe, owner);
    drop(manager);
    crate::toolbox::processes::termination::interrupt(&targets)?;
    let mut manager = self::manager()?.lock();
    if let Some(pipe) = manager.pipes.get_mut(endpoint.pipe)
    {
        pipe.ring.clear();
        if let Some(state) = &mut pipe.terminal
        {
            state.cancel_input();
        }
    }
    drop(manager);
    crate::machine::scheduler::pipe::wake(endpoint.pipe);
    Ok(RequestAction::Return)
}
