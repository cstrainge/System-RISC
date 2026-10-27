//! Owned SCEX sessions and snapshots for admitted native modules.
//!
//! A module-lifetime controller must own this client and drain its retirement
//! backlog. Guard destruction merely requests retirement: it performs no I/O
//! and allocates nothing. Only a checked resident acknowledgement completes a
//! release. Revocation, malformed replies and a stopped module never count as
//! cleanup success. Resident owner walks remain authoritative after quiesce.
//!
//! Dropping the entire client with outstanding records is last-resort
//! quarantine: their admitted backing and original provider are retained. An
//! owner walk does not free this forgotten SDK allocation. Use `shutdown` to
//! retain a recoverable client on refusal instead. Automatic production worker
//! lifecycle and immutable text capture are separate, unqualified integration
//! gates.
#![forbid(unsafe_code)]

use alloc::{sync::Arc, vec::Vec};
use core::{fmt,
           sync::atomic::{AtomicBool, AtomicU64, Ordering}};

use mac_abi::{ACCESS_DENIED, BAD_ABI, NO_MEMORY, SERVICE_BUSY};
use mac_driver::{Client, Provider};
use mac_scrap_extension_interface as wire;
pub use mac_scrap_extension_interface::{Content, Metadata};

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation
{
    Resolve,
    Open,
    Notice,
    Snapshot,
    Info,
    CloseSnapshot,
    Disconnect
}
impl Operation
{
    fn wire(self) -> u32
    {
        match self
        {
            Self::Resolve => 0,
            Self::Open => wire::OPEN,
            Self::Notice => wire::NOTICE,
            Self::Snapshot => wire::SNAPSHOT,
            Self::Info => wire::INFO,
            Self::CloseSnapshot => wire::CLOSE_PIN,
            Self::Disconnect => wire::DISCONNECT
        }
    }
}
impl fmt::Display for Operation
{
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        output.write_str(match self
                         {
                             Self::Resolve => "resolve",
                             Self::Open => "OPEN",
                             Self::Notice => "NOTICE",
                             Self::Snapshot => "SNAPSHOT",
                             Self::Info => "INFO",
                             Self::CloseSnapshot => "CLOSE_PIN",
                             Self::Disconnect => "DISCONNECT"
                         })
    }
}

/// Owned bounded diagnostics; reporting or retaining a cause cannot allocate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic
{
    bytes:  [u8; wire::MIN_REPLY - wire::REPLY_HEADER],
    length: usize
}
impl Diagnostic
{
    fn new(text: &str) -> Self
    {
        let mut diagnostic = Self { bytes:  [0; wire::MIN_REPLY - wire::REPLY_HEADER],
                                    length: text.len() };
        diagnostic.bytes[..text.len()].copy_from_slice(text.as_bytes());
        diagnostic
    }

    pub fn as_str(&self) -> &str
    {
        match core::str::from_utf8(&self.bytes[..self.length])
        {
            Ok(text) => text,
            Err(_) => "SCEX diagnostic UTF-8 invariant was violated"
        }
    }
}
impl fmt::Display for Diagnostic
{
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        output.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error
{
    Resolve(mac_driver::Error),
    NoMemory(Operation),
    ForeignGuard,
    Retiring,
    Broker
    {
        operation: Operation,
        status:    i32
    },
    Malformed
    {
        operation: Operation,
        detail:    &'static str
    },
    Resident
    {
        operation: Operation,
        status:    i32,
        cause:     Diagnostic
    }
}
impl Error
{
    pub fn status(&self) -> i32
    {
        match self
        {
            Self::Resolve(cause) => cause.0,
            Self::NoMemory(_) => NO_MEMORY,
            Self::ForeignGuard => ACCESS_DENIED,
            Self::Retiring => SERVICE_BUSY,
            Self::Broker { status,
                           .. }
            | Self::Resident { status,
                             .. } => *status,
            Self::Malformed { .. } => BAD_ABI
        }
    }

    pub fn is_busy(&self) -> bool
    {
        self.status() == SERVICE_BUSY
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Resolve(cause) =>
            {
                write!(output,
                       "SCEX dependency resolution failed ({}); declare the resident SCEX v1 \
                        service for this admitted INIT",
                       cause.0)
            }
            Self::NoMemory(operation) =>
            {
                write!(output,
                       "SCEX {operation} cleanup allocation was refused because memory is \
                        exhausted; no session or snapshot request ran")
            }
            Self::ForeignGuard =>
            {
                output.write_str("SCEX guard belongs to another client; retain its original \
                                  client and provider")
            }
            Self::Retiring =>
            {
                output.write_str("SCEX client or guard is retiring; retain the original cleanup \
                                  backlog and drain it before releasing module storage")
            }
            Self::Broker { operation,
                           status: SERVICE_BUSY } =>
            {
                write!(output,
                       "SCEX {operation} broker is busy (status {SERVICE_BUSY}) before effects; ")?;
                match operation
                {
                    Operation::Open | Operation::Snapshot =>
                    {
                        output.write_str("no grant was created, retry with the original client \
                                          after contention clears")
                    }
                    Operation::CloseSnapshot | Operation::Disconnect =>
                    {
                        output.write_str("the original grant and cleanup record remain retained, \
                                          retry unchanged after contention clears")
                    }
                    _ =>
                    {
                        output.write_str("retry unchanged with the original client after \
                                          contention clears")
                    }
                }
            }
            Self::Broker { operation,
                           status } =>
            {
                write!(output,
                       "SCEX {operation} broker refused status {status}; no release \
                        acknowledgement was received")
            }
            Self::Malformed { operation,
                              detail } =>
            {
                write!(output,
                       "SCEX {operation} returned malformed acknowledgement: {detail}; any \
                        uncertain resource remains retained")
            }
            Self::Resident { operation,
                             status,
                             cause } =>
            {
                write!(output, "SCEX {operation} resident refused status {status}: {cause}")
            }
        }
    }
}
impl core::error::Error for Error
{
}

struct Record
{
    provider: Arc<Provider>,
    parent:   Option<Arc<Record>>,
    handle:   AtomicU64,
    retired:  AtomicBool
}
struct Tracked
{
    record:    Arc<Record>,
    uncertain: Option<Error>
}

/// Unique observation/import session. It cannot be copied or cloned.
///
/// ```compile_fail
/// use mac_driver_alloc::scrap::Session;
/// fn duplicate(session: Session) { let other = session; drop(session); }
/// ```
pub struct Session
{
    record: Arc<Record>
}
impl Drop for Session
{
    fn drop(&mut self)
    {
        self.record.retired.store(true, Ordering::Release);
    }
}
/// Unique resident snapshot pin, retaining its original session and provider.
/// Dropping the session guard does not invalidate an outstanding snapshot: its
/// parent cannot disconnect until every child pin's release is acknowledged.
///
/// ```compile_fail
/// use mac_driver_alloc::scrap::Snapshot;
/// fn duplicate(snapshot: Snapshot) { let other = snapshot; drop(snapshot); }
/// ```
pub struct Snapshot
{
    record: Arc<Record>
}
impl Drop for Snapshot
{
    fn drop(&mut self)
    {
        self.record.retired.store(true, Ordering::Release);
    }
}

#[derive(Clone, Debug)]
pub struct Drain
{
    pub visited:   usize,
    pub attempted: usize,
    pub completed: usize,
    pub busy:      usize,
    pub pending:   usize,
    pub uncertain: usize,
    pub failure:   Option<Error>
}
impl Drain
{
    pub fn is_complete(&self) -> bool
    {
        self.pending == 0
    }
}

/// A refused shutdown still owns the original client and all cleanup records.
pub struct Shutdown
{
    client: Scrap,
    report: Drain
}
impl Shutdown
{
    pub fn report(&self) -> &Drain
    {
        &self.report
    }

    pub fn into_client(self) -> Scrap
    {
        self.client
    }
}
impl fmt::Debug for Shutdown
{
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        output.debug_struct("Shutdown").field("report", &self.report).finish_non_exhaustive()
    }
}
impl fmt::Display for Shutdown
{
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        write!(output,
               "SCEX shutdown retains {} records ({} uncertain); release outstanding guards and \
                retry bounded cleanup with the retained client",
               self.report.pending, self.report.uncertain)?;
        if let Some(cause) = &self.report.failure
        {
            write!(output, ": {cause}")?;
        }
        Ok(())
    }
}
impl core::error::Error for Shutdown
{
}

pub struct Scrap
{
    provider:  Arc<Provider>,
    records:   Vec<Tracked>,
    cursor:    usize,
    uncertain: usize,
    accepting: bool
}
impl Scrap
{
    pub fn new(client: Client) -> Result<Self>
    {
        let provider = client.resolve(wire::CLASS, wire::VERSION).map_err(Error::Resolve)?;
        let provider = Arc::try_new(provider).map_err(|_| Error::NoMemory(Operation::Resolve))?;
        Ok(Self { provider,
                  records: Vec::new(),
                  cursor: 0,
                  uncertain: 0,
                  accepting: true })
    }

    /// Includes live guards as well as queued retirement and quarantine.
    pub fn pending(&self) -> usize
    {
        self.records.len()
    }

    pub fn uncertain(&self) -> usize
    {
        self.uncertain
    }

    fn check(&self, record: &Record) -> Result<()>
    {
        if !Arc::ptr_eq(&self.provider, &record.provider)
        {
            return Err(Error::ForeignGuard);
        }
        if !self.accepting || record.retired.load(Ordering::Acquire)
        {
            return Err(Error::Retiring);
        }
        Ok(())
    }

    fn acquire(&mut self, parent: Option<Arc<Record>>, operation: Operation, input: &[u8])
               -> Result<Arc<Record>>
    {
        if !self.accepting
        {
            return Err(Error::Retiring);
        }
        // Both the queue entry and shared guard storage exist before the
        // resident can create anything. Drop never grows this queue.
        self.records.try_reserve(1).map_err(|_| Error::NoMemory(operation))?;
        let record =
            Arc::try_new(Record { provider: self.provider.clone(),
                                  parent,
                                  handle: AtomicU64::new(0),
                                  retired: AtomicBool::new(false) }).map_err(|_| {
                                                                        Error::NoMemory(operation)
                                                                    })?;
        self.records.push(Tracked { record:    record.clone(),
                                    uncertain: None });
        let mut output = [0; wire::MIN_REPLY];
        let result = request(*record.provider, operation, input, &mut output).and_then(|body| {
                         word(body).filter(|handle| *handle != 0)
                                   .ok_or(Error::Malformed { operation,
                                                             detail: "a new resident handle \
                                                                      must be one nonzero \
                                                                      eight-byte handle" })
                     });
        match result
        {
            Ok(handle) =>
            {
                record.handle.store(handle, Ordering::Release);
                Ok(record)
            }
            Err(cause) =>
            {
                // A well-formed terminal creation failure creates no grant.
                // Only outer Busy has that same guaranteed no-effect meaning;
                // every other broker refusal could follow an actual effect.
                if matches!(&cause, Error::Resident { .. })
                   || matches!(&cause,
                               Error::Broker { status: SERVICE_BUSY,
                                               .. })
                {
                    self.records.pop();
                }
                else
                {
                    record.retired.store(true, Ordering::Release);
                    if let Some(tracked) = self.records.last_mut()
                    {
                        tracked.uncertain = Some(cause.clone());
                        self.uncertain += 1;
                    }
                }
                Err(cause)
            }
        }
    }

    pub fn open(&mut self, writable: bool) -> Result<Session>
    {
        let input = u32::from(writable).to_le_bytes();
        self.acquire(None, Operation::Open, &input).map(|record| Session { record })
    }

    pub fn snapshot(&mut self, session: &Session) -> Result<Snapshot>
    {
        self.check(&session.record)?;
        let input = session.record.handle.load(Ordering::Acquire).to_le_bytes();
        self.acquire(Some(session.record.clone()), Operation::Snapshot, &input)
            .map(|record| Snapshot { record })
    }

    pub fn notice(&self, session: &Session) -> Result<Option<u64>>
    {
        self.check(&session.record)?;
        let input = session.record.handle.load(Ordering::Acquire).to_le_bytes();
        let mut output = [0; wire::MIN_REPLY];
        let body = request(*session.record.provider, Operation::Notice, &input, &mut output)?;
        let revision = word(body).ok_or(Error::Malformed { operation: Operation::Notice,
                                                           detail:    "NOTICE requires one \
                                                                       revision word" })?;
        Ok((revision != 0).then_some(revision))
    }

    pub fn info(&self, snapshot: &Snapshot) -> Result<Metadata>
    {
        self.check(&snapshot.record)?;
        let input = pair(&snapshot.record);
        let mut output = [0; wire::MIN_REPLY];
        let body = request(*snapshot.record.provider, Operation::Info, &input, &mut output)?;
        Metadata::decode(body).ok_or(Error::Malformed { operation: Operation::Info,
                                                        detail:    "INFO requires valid complete \
                                                                    snapshot metadata" })
    }

    /// Inspect at most `budget` records and attempt at most one release per
    /// visited record. Busy retains the exact record/provider for a later call.
    /// Child pins retain their session until acknowledged, without a lock or
    /// independently issued client-side object ID.
    pub fn drain(&mut self, budget: usize) -> Drain
    {
        let mut report = Drain { visited:   0,
                                 attempted: 0,
                                 completed: 0,
                                 busy:      0,
                                 pending:   self.records.len(),
                                 uncertain: self.uncertain,
                                 failure:   None };
        let mut without_progress = 0;
        for _ in 0..budget
        {
            if self.records.is_empty() || without_progress >= self.records.len()
            {
                break;
            }
            without_progress += 1;
            let index = self.cursor.min(self.records.len() - 1);
            self.cursor = (index + 1) % self.records.len();
            report.visited += 1;
            let tracked = &self.records[index];
            if let Some(cause) = &tracked.uncertain
            {
                if report.failure.is_none()
                {
                    report.failure = Some(cause.clone());
                }
                continue;
            }
            if self.accepting && !tracked.record.retired.load(Ordering::Acquire)
               || Arc::strong_count(&tracked.record) != 1
            {
                continue;
            }
            let operation = if tracked.record.parent.is_some()
            {
                Operation::CloseSnapshot
            }
            else
            {
                Operation::Disconnect
            };
            let input = pair(&tracked.record);
            let input = if tracked.record.parent.is_some() { &input[..] } else { &input[..8] };
            let mut output = [0; wire::MIN_REPLY];
            report.attempted += 1;
            let result =
                request(*tracked.record.provider, operation, input, &mut output).and_then(|body| {
                    if body.is_empty()
                    {
                        Ok(())
                    }
                    else
                    {
                        Err(Error::Malformed { operation,
                                               detail: "release acknowledgement must have no \
                                                        payload" })
                    }
                });
            match result
            {
                Ok(()) =>
                {
                    self.records.swap_remove(index);
                    report.completed += 1;
                    // A child's acknowledged removal can make a previously
                    // visited parent eligible within the remaining budget.
                    without_progress = 0;
                    self.cursor =
                        if self.records.is_empty() { 0 } else { index % self.records.len() };
                }
                Err(cause) =>
                {
                    if cause.is_busy()
                    {
                        report.busy += 1;
                    }
                    else
                    {
                        self.records[index].uncertain = Some(cause.clone());
                        self.uncertain += 1;
                    }
                    if report.failure.is_none()
                    {
                        report.failure = Some(cause);
                    }
                }
            }
        }
        report.pending = self.records.len();
        report.uncertain = self.uncertain;
        report
    }

    /// Close admission and request retirement of all records. Refusal returns
    /// this exact client for retry; live guards must also be dropped before
    /// their unique records can be released.
    pub fn shutdown(mut self, budget: usize) -> core::result::Result<(), Shutdown>
    {
        self.accepting = false;
        let report = self.drain(budget);
        if report.is_complete()
        {
            Ok(())
        }
        else
        {
            Err(Shutdown { client: self,
                           report })
        }
    }
}
impl Drop for Scrap
{
    fn drop(&mut self)
    {
        if !self.records.is_empty()
        {
            // Last-resort quarantine, matching resident completion retention.
            // Neither dropping this controller nor a resident owner walk is
            // an acknowledgement that frees this forgotten SDK allocation.
            core::mem::forget(core::mem::take(&mut self.records));
        }
    }
}

fn word(input: &[u8]) -> Option<u64>
{
    Some(u64::from_le_bytes(input.try_into().ok()?))
}
fn pair(record: &Record) -> [u8; 16]
{
    let mut input = [0; 16];
    let handle = record.handle.load(Ordering::Acquire);
    if let Some(parent) = &record.parent
    {
        input[..8].copy_from_slice(&parent.handle.load(Ordering::Acquire).to_le_bytes());
        input[8..].copy_from_slice(&handle.to_le_bytes());
    }
    else
    {
        input[..8].copy_from_slice(&handle.to_le_bytes());
    }
    input
}
fn request<'a>(provider: Provider, operation: Operation, input: &[u8],
               output: &'a mut [u8; wire::MIN_REPLY])
               -> Result<&'a [u8]>
{
    let length =
        provider.synchronous(operation.wire(), input, output).map_err(|cause| {
                                                                  Error::Broker { operation,
                                                                                  status: cause.0 }
                                                              })?;
    let bytes = output.get(..length)
                      .ok_or(Error::Malformed { operation,
                                                detail: "reply exceeded its admitted buffer" })?;
    match wire::Reply::decode(bytes)
    {
        Some(wire::Reply::Success(body)) => Ok(body),
        Some(wire::Reply::Failure { status,
                                    cause }) =>
        {
            Err(Error::Resident { operation,
                                  status,
                                  cause: Diagnostic::new(cause) })
        }
        None =>
        {
            Err(Error::Malformed { operation,
                                   detail: "invalid SCEX reply envelope" })
        }
    }
}
