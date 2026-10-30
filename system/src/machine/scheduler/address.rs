//! Actual-thread atomic waits. The scheduler serializes compare/queue/wake;
//! pending requests own storage, never another ThreadCore or a borrowed frame.
use core::{fmt,
           fmt::Write,
           sync::atomic::{AtomicBool, AtomicU32, Ordering}};

use mac_abi::{Status, address_wait as wire};

use super::{service::{RequestAction, RequestResult},
            *};
use crate::toolbox::{application_heap, handles::OwnerRef};

static INVALIDATED: AtomicBool = AtomicBool::new(false);

/// May run under a storage guard. The scheduler consumes the hint and checks
/// actual pending regions; a hint is neither cancellation nor reclamation.
pub(crate) fn changed()
{
    INVALIDATED.store(true, Ordering::Release);
    wake::request();
}

enum Region
{
    Heap(application_heap::wait::HeapWaitPin),
    Stack(super::StackWaitPin),
    Image(crate::images::ApplicationAtomic32)
}
impl Region
{
    fn prepare(executor: &crate::context::ApplicationExecutor<'_>, address: usize)
               -> Result<Self, Failure>
    {
        if address == 0
           || !address.is_multiple_of(core::mem::align_of::<AtomicU32>())
           || address.checked_add(core::mem::size_of::<AtomicU32>()).is_none()
        {
            return Err(Failure::Invalid("atomic address is null, unaligned or overflowing"));
        }
        if let Some(pin) = application_heap::wait::pin(executor, address).map_err(Failure::Heap)?
        {
            return Ok(Self::Heap(pin));
        }
        if let Some(pin) =
            super::pin_application_stack(executor, address, 4).map_err(Failure::Stack)?
        {
            return Ok(Self::Stack(pin));
        }
        crate::kernel::application_atomic32_pin(executor.owner(), executor.image(), address)
            .map(Self::Image)
            .map_err(|status| Failure::Resident(status,
                "atomic span is not in this process's live heap, stack or writable image"))
    }

    fn address(&self) -> usize
    {
        match self
        {
            Self::Heap(pin) => pin.address(),
            Self::Stack(pin) => pin.address(),
            Self::Image(pin) => pin.address()
        }
    }

    fn accepting(&self) -> bool
    {
        match self
        {
            Self::Heap(pin) => pin.accepting(),
            Self::Stack(pin) => pin.accepting(),
            Self::Image(pin) => pin.still_admitted()
        }
    }

    fn matches(&self, other: &Self) -> bool
    {
        match (self, other)
        {
            (Self::Heap(a), Self::Heap(b)) => a.identity() == b.identity(),
            (Self::Stack(a), Self::Stack(b)) =>
            {
                a.identity() == b.identity() && a.address() == b.address()
            }
            (Self::Image(a), Self::Image(b)) => a.key() == b.key(),
            _ => false
        }
    }

    fn load(&self) -> u32
    {
        // The only raw access in the wait service. Each private variant owns
        // the entire checked/aligned writable span in the actual Process.
        // The native API separately requires the caller to retain a live
        // AtomicU32 object: a physical extent pin cannot prove Rust object
        // lifetime or forgive non-atomic concurrent access.
        unsafe { (&*(self.address() as *const AtomicU32)).load(Ordering::Relaxed) }
    }

    fn release(self)
    {
        match self
        {
            Self::Heap(pin) => pin.release(),
            Self::Stack(pin) => drop(pin),
            Self::Image(pin) => drop(pin)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Inputs
{
    address:     usize,
    expected:    u32,
    flags:       u32,
    seconds:     u64,
    nanoseconds: u32
}
impl Inputs
{
    fn read(call: &wire::Call) -> Self
    {
        Self { address:     call.address,
               expected:    call.expected,
               flags:       call.flags,
               seconds:     call.seconds,
               nanoseconds: call.nanoseconds }
    }
}

pub(super) struct Pending
{
    inputs:   Inputs,
    identity: service::RequestIdentity,
    region:   Region,
    outcome:  Option<Result<u32, Status>>
}
impl Pending
{
    fn release(self)
    {
        self.region.release();
    }
}

enum Failure
{
    Invalid(&'static str),
    Context(crate::context::ApplicationExecutorError),
    Heap(application_heap::Error),
    Stack(super::StackPinError),
    Resident(Status, &'static str)
}
impl Failure
{
    fn status(&self) -> Status
    {
        match self
        {
            Self::Invalid(_) => mac_abi::BAD_ARGUMENT,
            Self::Context(cause) => cause.status(),
            Self::Heap(cause) => cause.status(),
            Self::Stack(cause) => cause.status(),
            Self::Resident(status, _) => *status
        }
    }
}
impl fmt::Display for Failure
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Invalid(detail) => write!(f, "address wait: {detail}"),
            Self::Context(cause) => write!(f, "address wait: {cause}"),
            Self::Heap(cause) => write!(f, "address wait heap: {cause}"),
            Self::Stack(cause) => write!(f, "address wait stack: {cause}"),
            Self::Resident(status, detail) => write!(f, "address wait: {detail} (status {status})")
        }
    }
}
struct Detail<'a>(&'a mut [u8], usize);
impl fmt::Write for Detail<'_>
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        let mut count = text.len().min(self.0.len() - self.1);
        while !text.is_char_boundary(count)
        {
            count -= 1;
        }
        self.0[self.1..self.1 + count].copy_from_slice(&text.as_bytes()[..count]);
        self.1 += count;
        Ok(())
    }
}

struct Subject
{
    owner:  OwnerRef,
    thread: u64
}
impl Subject
{
    fn current() -> Result<Self, Failure>
    {
        crate::context::with_application_executor(|executor| {
            Self { owner:  executor.owner().clone(),
                   thread: executor.thread() }
        }).map_err(Failure::Context)
    }

    fn matches(&self, thread: &Thread) -> bool
    {
        self.thread == thread.id
        && self.owner.accepting()
        && self.owner.owner().incarnation() == thread.core.context.identity().app.0
        && thread.state.get().live
        && !thread.state.get().retired
    }
}

pub(crate) fn request(call: &mut wire::Call, identity: service::RequestIdentity,
                      suspension: Result<(), Status>)
                      -> RequestResult
{
    call.outcome = 0;
    call.awakened = 0;
    call.detail_length = 0;
    let result = crate::context::without_recovery(|| operation(call, identity, suspension));
    match result
    {
        Ok(action) =>
        {
            RequestResult { status: mac_abi::OK,
                            action }
        }
        Err(cause) =>
        {
            cancel_current();
            let mut detail = Detail(&mut call.detail, 0);
            let _ = write!(detail, "{cause}");
            call.detail_length = detail.1 as u32;
            RequestResult { status: cause.status(),
                            action: RequestAction::Return }
        }
    }
}

fn operation(call: &mut wire::Call, identity: service::RequestIdentity,
             suspension: Result<(), Status>)
             -> Result<RequestAction, Failure>
{
    if call.reserved != [0; 2] || call.reserved2 != 0
    {
        return Err(Failure::Invalid("reserved fields must be zero"));
    }
    match call.operation
    {
        wire::WAIT32 if call.flags & !wire::FINITE == 0 =>
        {
            if call.nanoseconds >= 1_000_000_000
               || (call.flags & wire::FINITE == 0 && (call.seconds != 0 || call.nanoseconds != 0))
            {
                return Err(Failure::Invalid("invalid finite/infinite timeout"));
            }
        }
        wire::WAKE32 if call.flags & !wire::WAKE_ALL == 0 =>
        {
            if call.expected != 0 || call.seconds != 0 || call.nanoseconds != 0
            {
                return Err(Failure::Invalid("wake contains wait-only fields"));
            }
        }
        _ => return Err(Failure::Invalid("unknown operation or flags"))
    }
    if call.operation == wire::WAIT32
    {
        suspension.map_err(|status| {
                      Failure::Resident(status, "current frame cannot suspend for an atomic wait")
                  })?;
    }
    let subject = Subject::current()?;
    let inputs = Inputs::read(call);
    if call.operation != wire::WAIT32
    {
        let scheduler = SCHEDULER.lock();
        if scheduler.thread(current())
                    .filter(|thread| subject.matches(thread))
                    .is_some_and(|thread| thread.core.address.lock().is_some())
        {
            return Err(Failure::Invalid("suspended WAIT32 changed its operation"));
        }
    }
    if call.operation == wire::WAIT32
       && let Some(action) = resume(&subject, inputs, identity, call)?
    {
        return Ok(action);
    }
    let region = crate::context::with_application_executor(|executor| {
                     if executor.thread() != subject.thread
                        || executor.owner().owner() != subject.owner.owner()
                     {
                         return Err(Failure::Resident(mac_abi::WRONG_THREAD,
                                                      "executor changed during admission"));
                     }
                     Region::prepare(executor, call.address)
                 }).map_err(Failure::Context)??;
    if call.operation == wire::WAKE32
    {
        call.awakened = wake_region(&subject, &region, call.flags & wire::WAKE_ALL != 0)?;
        return Ok(RequestAction::Return);
    }
    let deadline = if call.flags & wire::FINITE != 0
    {
        Some(service::relative_deadline(call.seconds, call.nanoseconds).map_err(|cause| {
                 Failure::Resident(cause.status(), "timeout exceeds the admitted clock range")
             })?)
    }
    else
    {
        None
    };
    enqueue(&subject, inputs, identity, region, deadline, call)
}

fn resume(subject: &Subject, inputs: Inputs, identity: service::RequestIdentity,
          call: &mut wire::Call)
          -> Result<Option<RequestAction>, Failure>
{
    let detached = {
        let scheduler = SCHEDULER.lock();
        let thread = scheduler.thread(current())
                              .filter(|thread| subject.matches(thread))
                              .ok_or(Failure::Resident(mac_abi::WRONG_THREAD,
                                                       "no live matching application thread"))?;
        let mut pending = thread.core.address.lock();
        let Some(wait) = pending.as_ref()
        else
        {
            return Ok(None);
        };
        if wait.inputs != inputs || wait.identity != identity
        {
            return Err(Failure::Invalid("suspended wait input changed before completion"));
        }
        if wait.outcome.is_none()
        {
            return Ok(Some(RequestAction::Replay));
        }
        pending.take()
    };
    let Some(wait) = detached
    else
    {
        return Err(Failure::Resident(mac_abi::BAD_ABI, "completed wait lost its owning record"));
    };
    let outcome = wait.outcome;
    wait.release(); // Actual storage release is outside SCHEDULER/pending guards.
    match outcome
    {
        Some(Ok(outcome)) =>
        {
            call.outcome = outcome;
            Ok(Some(RequestAction::Return))
        }
        Some(Err(status)) => Err(Failure::Resident(status, "wait backing or process was retired")),
        None => Err(Failure::Resident(mac_abi::BAD_ABI, "completed wait has no terminal outcome"))
    }
}

fn enqueue(subject: &Subject, inputs: Inputs, identity: service::RequestIdentity, region: Region,
           deadline: Option<u64>, call: &mut wire::Call)
           -> Result<RequestAction, Failure>
{
    // `region` is an argument: any refusal drops it after local guards leave.
    let scheduler = SCHEDULER.lock();
    let thread = scheduler.thread(current())
                          .filter(|thread| subject.matches(thread))
                          .ok_or(Failure::Resident(mac_abi::WRONG_THREAD,
                                                   "thread stopped before wait admission"))?;
    service::suspension_allowed(thread.state.get()).map_err(|status| {
                                                       Failure::Resident(status,
                                                                         "current invocation \
                                                                          cannot suspend")
                                                   })?;
    let mut pending = thread.core.address.lock();
    if pending.is_some()
    {
        return Err(Failure::Resident(mac_abi::SERVICE_BUSY, "thread already owns an atomic wait"));
    }
    if !region.accepting()
    {
        return Err(Failure::Resident(mac_abi::OPERATION_CANCELLED,
                                     "atomic backing closed during admission"));
    }
    if region.load() != inputs.expected
    {
        call.outcome = wire::VALUE_CHANGED;
        return Ok(RequestAction::Return);
    }
    if deadline.is_some_and(|deadline| crate::platform::ticks() >= deadline)
    {
        call.outcome = wire::TIMED_OUT;
        return Ok(RequestAction::Return);
    }
    *pending = Some(Pending { inputs,
                              identity,
                              region,
                              outcome: None });
    thread.update(|state| {
              state.waiting = Some(Wait::Address { deadline });
              state.woken = 0;
          });
    Ok(RequestAction::Replay)
}

fn wake_region(subject: &Subject, region: &Region, all: bool) -> Result<u32, Failure>
{
    let scheduler = SCHEDULER.lock();
    if !scheduler.thread(current()).is_some_and(|thread| subject.matches(thread))
       || !region.accepting()
    {
        return Err(Failure::Resident(mac_abi::OPERATION_CANCELLED,
                                     "wake subject or backing retired"));
    }
    // Bound the result before delivering any wake. The iteration below cannot
    // visit more rows than this preflighted canonical scheduler storage.
    if scheduler.threads.len() > u32::MAX as usize
    {
        return Err(Failure::Resident(mac_abi::NO_MEMORY,
                                     "admitted thread population exceeds native wake-count \
                                      capacity"));
    }
    let mut count = 0u32;
    for (_, thread) in scheduler.iter()
    {
        if !thread.state.get().live
           || !matches!(thread.state.get().waiting, Some(Wait::Address { .. }))
           || thread.core.context.identity().app.0 != subject.owner.owner().incarnation()
        {
            continue;
        }
        let mut pending = thread.core.address.lock();
        let Some(wait) = pending.as_mut()
        else
        {
            continue;
        };
        if wait.outcome.is_none() && wait.region.matches(region)
        {
            wait.outcome = Some(Ok(wire::WOKEN));
            ready(thread);
            count += 1;
            if !all
            {
                break;
            }
        }
    }
    Ok(count)
}

fn ready(thread: &Thread)
{
    thread.update(|state| {
              state.waiting = None;
              state.woken = 1;
          });
    wake::runnable(thread.state.get().requires_primary());
}

/// Called with SCHEDULER held. No physical storage is dropped here. A close
/// before enqueue fails its accepting check; a later close leaves this hint.
pub(super) fn take_invalidations() -> bool
{
    INVALIDATED.swap(false, Ordering::AcqRel)
}

/// Runs in the scheduler's existing deadline traversal, not a second scan.
pub(super) fn observe(thread: &Thread, now: u64, invalidated: bool)
{
    if thread.state.get().retired
    {
        return;
    }
    let Some(Wait::Address { deadline }) = thread.state.get().waiting
    else
    {
        return;
    };
    if !invalidated && !deadline.is_some_and(|deadline| now >= deadline)
    {
        return;
    }
    let mut pending = thread.core.address.lock();
    let Some(wait) = pending.as_mut()
    else
    {
        return;
    };
    if wait.outcome.is_some()
    {
        return;
    }
    if invalidated && !wait.region.accepting()
    {
        wait.outcome = Some(Err(mac_abi::OPERATION_CANCELLED));
        ready(thread);
    }
    else if deadline.is_some_and(|deadline| now >= deadline)
    {
        wait.outcome = Some(Ok(wire::TIMED_OUT));
        ready(thread);
    }
}

/// A stopped individual child cannot replay its suspended request. Detach its
/// original pin before checking the THRD's stack-reader reclamation barrier.
pub(super) fn release_retired()
{
    let mut after = 0;
    loop
    {
        let detached = {
            let scheduler = SCHEDULER.lock();
            let Some((slot, thread)) = scheduler.iter().find(|(slot, thread)| {
                                                           *slot >= after
                                                           && thread.state.get().retired
                                                           && thread.core.address.lock().is_some()
                                                       })
            else
            {
                break;
            };
            after = slot + 1;
            thread.update(|state| {
                      if matches!(state.waiting, Some(Wait::Address { .. }))
                      {
                          state.waiting = None;
                      }
                  });
            thread.core.address.lock().take()
        };
        if let Some(pending) = detached
        {
            pending.release();
        }
    }
}

/// Stop closes admission first. Detach one original wait at a time, releasing
/// real backing only after the scheduler guard ends; no allocation during reap.
pub(super) fn cancel_owner(owner: u64)
{
    let mut after = 0;
    loop
    {
        let detached = {
            let scheduler = SCHEDULER.lock();
            let Some((slot, thread)) = scheduler.iter().find(|(slot, thread)| {
                                                           *slot >= after
                                                           && thread.core.context.identity().app.0
                                                              == owner
                                                           && thread.core.address.lock().is_some()
                                                       })
            else
            {
                break;
            };
            after = slot + 1;
            thread.update(|state| {
                      if matches!(state.waiting, Some(Wait::Address { .. }))
                      {
                          state.waiting = None;
                      }
                  });
            thread.core.address.lock().take()
        };
        if let Some(pending) = detached
        {
            pending.release();
        }
    }
}

/// Terminal refusal settles this invocation's actual pending owner as well.
/// A malformed replay cannot leave the next valid wait permanently Busy.
pub(crate) fn cancel_current()
{
    let Ok(subject) = Subject::current()
    else
    {
        return;
    };
    let detached = {
        let scheduler = SCHEDULER.lock();
        let Some(thread) = scheduler.thread(current()).filter(|thread| subject.matches(thread))
        else
        {
            return;
        };
        thread.update(|state| {
                  if matches!(state.waiting, Some(Wait::Address { .. }))
                  {
                      state.waiting = None;
                  }
              });
        thread.core.address.lock().take()
    };
    if let Some(pending) = detached
    {
        pending.release();
    }
}
