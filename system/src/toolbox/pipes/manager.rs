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

/// Resident pipe object. Owned by the System, not by any single process: the
/// last endpoint of either kind decides when it may be destroyed.
struct Pipe
{
    ring:    Ring,
    readers: u32,
    writers: u32
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
    created: u64
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
    pipe:     u64,
    reader:   bool,
    blocking: bool
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

pub(crate) fn specification(handles: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if handles { *b"PIPE" } else { *b"PPST" }),
                    label:    if handles { "Pipe endpoints" } else { "Named and anonymous pipes" },
                    provider: "System Pipes",
                    capacity: CapacityPolicy::HardBound { slots:  if handles
                                                          {
                                                              256
                                                          }
                                                          else
                                                          {
                                                              wire::MAX_PIPES
                                                          },
                                                          reason: if handles
                                                          {
                                                              "Two endpoints per anonymous pipe \
                                                               plus one writer per connection"
                                                          }
                                                          else
                                                          {
                                                              "Bounded resident pipe population"
                                                          } },
                    source:   Source::Resident }
}

/// Registered cleanup for one abandoned endpoint. The handle table invalidates
/// before this runs; the endpoint count is decremented exactly once and the
/// remaining endpoints learn about the close on their next operation.
pub(crate) struct ReleasePipes;
impl Release<crate::toolbox::runtime::Toolbox, crate::toolbox::handles::cleanup::Failure>
    for ReleasePipes
{
    fn release(&self, _: &mut crate::toolbox::runtime::Toolbox, ticket: ReleasedResource)
               -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        release_endpoint(ticket).map_err(|ticket| {
            crate::toolbox::handles::cleanup::Failure::context_unavailable(ticket, "Pipes")
        })
    }
}

fn manager() -> Result<&'static Spinlock<Manager>, Status>
{
    MANAGER.try_call_once(|| {
               let (pipe, endpoint) = crate::toolbox::handles::pipe_kinds()?;
               Ok::<_, Status>(Spinlock::new(Manager { pipes:     Store::registered(pipe),
                                                       endpoints: Store::registered(endpoint),
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
    let counter = NEXT_CNID.try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                             value.checked_add(1)
                         })
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
    Some(Row { cnid: entry.cnid,
               created: entry.created,
               name: entry.name })
}

/// Registry row by item name, using the same comparison as `CONNECT`.
pub(crate) fn find(text: &[u8]) -> Option<Row>
{
    let manager = manager().ok()?;
    let manager = manager.lock();
    let entry = manager.named.iter().find(|entry| same_name(named(entry), text))?;
    Some(Row { cnid: entry.cnid,
               created: entry.created,
               name: entry.name })
}

/// Registry row by synthetic identity.
pub(crate) fn row_of(cnid: u32) -> Option<Row>
{
    let manager = manager().ok()?;
    let manager = manager.lock();
    let entry = manager.named.iter().find(|entry| entry.cnid == cnid)?;
    Some(Row { cnid: entry.cnid,
               created: entry.created,
               name: entry.name })
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
    if call.reserved != [0; 2] || call.nonblocking > 1
    {
        return Err(BAD_ARGUMENT);
    }
    let owner = crate::context::current_owner()?;
    // A resumed call must be the same request; a changed one is refused before
    // it can consume a different handle than the one that parked.
    let resumed = crate::machine::scheduler::pipe::begin(identity, call.handle, call.operation)?;
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
                if call.name_length != 0
                {
                    return Err(BAD_ARGUMENT);
                }
                None
            };
            create(call, &owner, name)
        }
        wire::CONNECT =>
        {
            if resumed || call.handle != 0 || !output.is_empty() || !input.is_empty()
            {
                return Err(BAD_ARGUMENT);
            }
            let name =
                decode_name(call.name.get(..call.name_length as usize).ok_or(BAD_ARGUMENT)?)?;
            connect(call, &owner, name)
        }
        wire::READ =>
        {
            if call.handle == 0 || call.name_length != 0 || !input.is_empty()
            {
                return Err(BAD_ARGUMENT);
            }
            read(call, &owner, output, identity, suspension)
        }
        wire::WRITE =>
        {
            if call.handle == 0 || call.name_length != 0 || !output.is_empty()
            {
                return Err(BAD_ARGUMENT);
            }
            write(call, &owner, input, identity, suspension)
        }
        wire::SET_BLOCKING =>
        {
            if resumed || call.handle == 0 || !output.is_empty() || !input.is_empty()
            {
                return Err(BAD_ARGUMENT);
            }
            set_blocking(call, &owner)
        }
        wire::CLOSE =>
        {
            if resumed || call.handle == 0 || !output.is_empty() || !input.is_empty()
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
    if let Some(name) = &name
    {
        for entry in &manager.named
        {
            if same_name(named(entry), &name.1[..name.0 as usize])
            {
                return Err(ALREADY_EXISTS);
            }
        }
        // Preflight the registry slot before any pipe or endpoint exists, so a
        // refused insertion cannot leave a half-registered name. The identity
        // is taken before the objects so exhaustion is refused before them
        // too; a failed creation burns an identity rather than reusing one.
        Gate::with_system_allocation(|| manager.named.try_reserve(1)).map_err(|_| NO_MEMORY)?;
        cnid = Some(free_cnid().ok_or(NO_MEMORY)?);
    }
    let pipe = manager.pipes
                      .insert(&system,
                              Pipe { ring,
                                     readers: 1,
                                     writers: 1 })
                      .map_err(status)?;
    let reader = match manager.endpoints.insert(owner,
                                                Endpoint { pipe,
                                                           reader: READER,
                                                           blocking: call.nonblocking == 0 })
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
                                                           blocking: call.nonblocking == 0 })
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
                                   created: registration_time() });
        manager.bump();
    }
    call.reader = reader;
    call.writer = writer;
    Ok(RequestAction::Return)
}

fn connect(call: &mut wire::Call, owner: &OwnerRef, name: Name) -> Result<RequestAction, Status>
{
    let manager = manager()?;
    let mut manager = manager.lock();
    let Some(pipe) = manager.named
                            .iter()
                            .find(|entry| same_name(named(entry), &name.1[..name.0 as usize]))
                            .map(|entry| entry.pipe)
    else
    {
        return Err(NOT_FOUND);
    };
    let writer = manager.endpoints
                        .insert(owner,
                                Endpoint { pipe,
                                           reader: WRITER,
                                           blocking: call.nonblocking == 0 })
                        .map_err(status)?;
    let Some(pipe) = manager.pipes.get_mut(pipe)
    else
    {
        let _ = manager.endpoints.remove(writer);
        return Err(NOT_FOUND);
    };
    pipe.writers = pipe.writers.checked_add(1).ok_or(BAD_ARGUMENT)?;
    call.writer = writer;
    Ok(RequestAction::Return)
}

fn read(call: &mut wire::Call, owner: &OwnerRef, output: &mut [u8], identity: RequestIdentity,
        suspension: Result<(), Status>)
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
        if pipe.ring.available() > 0
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
        else if !endpoint.blocking
        {
            (WOULD_BLOCK, None)
        }
        else
        {
            crate::machine::scheduler::pipe::park(endpoint.pipe, call.handle, call.operation,
                                                  identity, suspension)?;
            return Ok(RequestAction::Replay);
        }
    };
    drop(manager);
    if let Some(pipe) = wake
    {
        crate::machine::scheduler::pipe::wake(pipe);
    }
    if status == OK { Ok(RequestAction::Return) } else { Err(status) }
}

fn write(call: &mut wire::Call, owner: &OwnerRef, input: &[u8], identity: RequestIdentity,
         suspension: Result<(), Status>)
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
                                                      identity, suspension)?;
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
    retire_endpoint(&mut manager, endpoint);
    drop(manager);
    // Readers see end of stream, writers see broken pipe, and a destroyed pipe
    // also releases anyone still parked on it.
    crate::machine::scheduler::pipe::wake(endpoint.pipe);
    Ok(RequestAction::Return)
}

/// Apply one endpoint's departure to its pipe. A pipe with no endpoints left is
/// destroyed here; the caller wakes the pipe after the manager lock ends.
fn retire_endpoint(manager: &mut Manager, endpoint: Endpoint)
{
    let Some(pipe) = manager.pipes.get_mut(endpoint.pipe)
    else
    {
        return;
    };
    if endpoint.reader
    {
        pipe.readers = pipe.readers.saturating_sub(1);
        if pipe.readers == 0
        {
            pipe.ring.clear();
        }
    }
    else
    {
        pipe.writers = pipe.writers.saturating_sub(1);
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
fn release_endpoint(ticket: ReleasedResource) -> Result<(), ReleasedResource>
{
    let Ok(manager) = manager()
    else
    {
        return Err(ticket);
    };
    let mut manager = manager.lock();
    let removed = match manager.endpoints.release(ticket)
    {
        Ok(removed) => removed,
        Err(error) => return Err(error.into_ticket())
    };
    let endpoint = *removed;
    drop(removed);
    retire_endpoint(&mut manager, endpoint);
    drop(manager);
    crate::machine::scheduler::pipe::wake(endpoint.pipe);
    Ok(())
}
