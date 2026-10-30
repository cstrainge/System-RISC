//! Native children share a genuine Process and runtime roots, not a module
//! worker identity. Join rights retain completion metadata, never ThreadCore.
use alloc::boxed::Box;
use core::{fmt,
           sync::atomic::{AtomicU32, AtomicU64}};

use mac_abi::{Status, native_threads as wire};
use mac_object_table::{domain::{DomainKind, PoolError},
                       registry::{Release, Source, Specification},
                       store::{PreparedError, PublicationPermit}};

use super::*;
mod manager;
mod creation;
mod join;
mod failure;
pub(crate) use creation::create;
pub(crate) use failure::{fail_owner, failure_quiescent};
pub(crate) use join::{cancel_current, cancel_owner, detach, join, query};
pub(crate) use manager::{ReleaseRights, initialize, pump_rights, specification};
pub(super) struct Completion
{
    owner:  OwnerRef,
    thread: AtomicU64,
    phase:  AtomicU32
}
impl Completion
{
    pub(super) fn retire(&self)
    {
        if self.phase.load(Ordering::Acquire) != wire::RETURNING
        {
            self.phase.store(wire::ABORTED, Ordering::Release);
        }
    }

    fn new(owner: &OwnerRef) -> Self
    {
        Self { owner:  owner.clone(),
               thread: AtomicU64::new(0),
               phase:  AtomicU32::new(wire::PREPARING) }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Claim
{
    Open,
    Joining(u64),
    Consumed
}
struct Right
{
    completion: Arc<Completion>,
    claimant:   Mutex<Claim>,
    anchor:     Mutex<Option<Box<Retiring>>>,
    signal:     Arc<ReadSignal>
}
struct ReadSignal
{
    deleted: AtomicBool,
    owner:   OwnerRef
}
struct RightRead
{
    key:       mac_object_table::domain::ResourceKey,
    right:     Arc<Right>,
    _released: ReadRelease
}
struct ReadRelease
{
    signal: Arc<ReadSignal>,
    notify: ReadNotify
}
struct ReadNotify
{
    needed:  bool,
    deleted: bool,
    owner:   OwnerRef
}
impl RightRead
{
    fn new(right: Arc<Right>, key: mac_object_table::domain::ResourceKey) -> Self
    {
        let _released = ReadRelease { signal: right.signal.clone(),
                                      notify: ReadNotify { needed:  false,
                                                           deleted: false,
                                                           owner:   right.signal.owner.clone() } };
        Self { key,
               right,
               _released }
    }
}
impl core::ops::Deref for RightRead
{
    type Target = Right;

    fn deref(&self) -> &Right
    {
        &self.right
    }
}
impl Drop for ReadRelease
{
    fn drop(&mut self)
    {
        self.notify.deleted = self.signal.deleted.load(Ordering::Acquire);
        self.notify.needed = self.notify.deleted || !self.signal.owner.accepting();
    }
}
impl Drop for ReadNotify
{
    fn drop(&mut self)
    {
        if self.needed
        {
            if self.deleted
            {
                manager::changed();
            }
            if !self.owner.accepting()
            {
                crate::toolbox::processes::retirement::resources_released();
            }
        }
    }
}
struct Retiring
{
    removal: Option<mac_object_table::store::Removal<Arc<Right>,
                                                     crate::toolbox::handles::InvocationGate>>,
    next:    Option<Box<Retiring>>
}
pub(super) struct PendingJoin
{
    identity: service::RequestIdentity,
    id:       u64,
    waiter:   u64,
    right:    RightRead
}
#[derive(Debug)]
pub(crate) enum Error
{
    Invalid(&'static str),
    Context(crate::context::ApplicationExecutorError),
    Tls(crate::toolbox::native_threads::Error),
    Accounting(accounting::Error),
    Pool(PoolError),
    Publication(PreparedError),
    Deadline(service::DeadlineError),
    Admission
    {
        status: Status,
        stage:  &'static str
    },
    Metadata(&'static str),
    Missing,
    Foreign,
    SelfJoin,
    Detached,
    Busy,
    Aborted,
    Stopped,
    MainExit
}
impl Error
{
    pub(crate) fn status(&self) -> Status
    {
        match self {
            Self::Context(cause)=>cause.status(),Self::Tls(cause)=>cause.status(),Self::Accounting(cause)=>cause.status(),Self::Deadline(cause)=>cause.status(),
            Self::Pool(cause)=>crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(*cause)),
            Self::Publication(PreparedError::Pool(cause))=>crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(*cause)),
            Self::Publication(_)=>mac_abi::BAD_ABI,Self::Admission{status,..}=>*status,
            Self::Metadata(_)=>mac_abi::NO_MEMORY,Self::Missing=>mac_abi::NOT_FOUND,Self::Foreign=>mac_abi::ACCESS_DENIED,
            Self::SelfJoin|Self::Invalid(_)|Self::MainExit=>mac_abi::BAD_ARGUMENT,Self::Busy=>mac_abi::SERVICE_BUSY,
            Self::Aborted|Self::Stopped|Self::Detached=>mac_abi::OPERATION_CANCELLED
        }
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Invalid(reason) => write!(out, "native thread request refused: {reason}"),
            Self::Context(cause) => cause.fmt(out),
            Self::Tls(cause) => cause.fmt(out),
            Self::Accounting(cause) => cause.fmt(out),
            Self::Pool(cause) => cause.fmt(out),
            Self::Publication(cause) => cause.fmt(out),
            Self::Deadline(cause) => cause.fmt(out),
            Self::Admission { status,
                              stage } =>
            {
                write!(out,
                       "native child {stage} refused with status {status}; original ownership \
                        retained")
            }
            Self::Metadata(stage) =>
            {
                write!(out, "original Process Kernel budget refused child {stage}")
            }
            Self::Missing =>
            {
                out.write_str("native join right is stale, detached, consumed or absent")
            }
            Self::Foreign => out.write_str("native join right belongs to another genuine Process"),
            Self::SelfJoin => out.write_str("a native thread cannot join its own execution"),
            Self::Detached =>
            {
                out.write_str("the original native join right was detached; the pending join was \
                               cancelled")
            }
            Self::Busy => out.write_str("another actual thread already owns this join operation"),
            Self::Aborted =>
            {
                out.write_str("native child was aborted; normal completion and TLS execution are \
                               not claimed")
            }
            Self::Stopped =>
            {
                out.write_str("Process runtime admission is closed or thread is stopping")
            }
            Self::MainExit =>
            {
                out.write_str("native child EXIT requires a child row; process main uses native \
                               process exit")
            }
        }
    }
}
impl From<PoolError> for Error
{
    fn from(cause: PoolError) -> Self
    {
        Self::Pool(cause)
    }
}
fn current_row<'a>(scheduler: &'a Scheduler, executor: &crate::context::ApplicationExecutor<'_>)
                   -> Result<&'a Thread, Error>
{
    let thread = scheduler.thread(current())
                          .ok_or(Error::Admission { status: mac_abi::WRONG_THREAD,
                                                    stage:  "current THRD" })?;
    if thread.id != executor.thread()
       || thread.core.binding.owner() != executor.owner().owner().incarnation()
    {
        return Err(Error::Foreign);
    }
    Ok(thread)
}
/// Called while SCHEDULER owns the actual complete old-frame handoff.
pub(super) fn execution_released(scheduler: &Scheduler, thread: &Thread)
{
    let Some(completion) = thread.core.application.as_ref()
    else
    {
        return;
    };
    if thread.state.get().claimed_by.is_none()
       && completion.phase.load(Ordering::Acquire) == wire::RETURNING
    {
        completion.phase.store(wire::COMPLETE, Ordering::Release);
        for (_, waiter) in scheduler.iter()
        {
            if matches!(waiter.state.get().waiting,Some(Wait::NativeJoin{request}) if request==thread.id)
            {
                waiter.update(|state| {
                          state.waiting = None;
                          state.woken = 1;
                      });
            }
        }
    }
}
/// Normal EXIT invalidates one genuine THRD while preserving its original row,
/// stack and ticket until the actual noncurrent reaper can acknowledge them.
pub(crate) fn exit(executor: &crate::context::ApplicationExecutor<'_>) -> Result<(), Error>
{
    let finished = executor.with_tls(|slots| slots.finished()).map_err(Error::Tls)?;
    if !finished
    {
        return Err(Error::Invalid("child returned before actual TLS FINISH"));
    }
    service::exit_allowed().map_err(|status| {
                               Error::Admission { status,
                                                  stage: "child EXIT Host guard" }
                           })?;
    let scheduler = SCHEDULER.lock();
    let thread = current_row(&scheduler, executor)?;
    let completion = thread.core.application.as_ref().ok_or(Error::MainExit)?;
    if completion.phase.load(Ordering::Acquire) != wire::RUNNING
    {
        return Err(Error::Stopped);
    }
    target_retire(&scheduler, thread, executor.owner(), true)?;
    Ok(())
}
fn target_retire(scheduler: &Scheduler, thread: &Thread, owner: &OwnerRef, normal: bool)
                 -> Result<(), Error>
{
    if thread.state.get().retired
    {
        return Err(Error::Stopped);
    }
    let mut release = thread.core.release.lock();
    if release.is_some()
    {
        return Err(Error::Busy);
    }
    let key =
        scheduler.records.as_ref().and_then(|store| store.key(thread.id)).ok_or(Error::Missing)?;
    let ticket = owner.invalidate(key).map_err(|_| Error::Stopped)?;
    *release = Some(ticket);
    if let Some(completion) = thread.core.application.as_ref()
    {
        // Only after acquiring the genuine original THRD ticket; no typed
        // refusal remains. Normal retirement must never transiently publish
        // ABORTED to a concurrent QUERY outside this scheduler turn.
        completion.phase
                  .store(if normal { wire::RETURNING } else { wire::ABORTED }, Ordering::Release);
    }
    retire(thread);
    Ok(())
}
pub(crate) fn sleep(executor: &crate::context::ApplicationExecutor<'_>, seconds: u64,
                    nanoseconds: u32)
                    -> Result<(), Error>
{
    let deadline = service::relative_deadline(seconds, nanoseconds).map_err(Error::Deadline)?;
    if seconds == 0 && nanoseconds == 0
    {
        return Ok(());
    }
    let scheduler = SCHEDULER.lock();
    let thread = current_row(&scheduler, executor)?;
    service::suspension_allowed(thread.state.get()).map_err(|status| {
                                                       Error::Admission { status,
                                                                          stage:
                                                                              "sleep admission" }
                                                   })?;
    thread.update(|state| {
              state.waiting = Some(Wait::Sleep { deadline });
              state.woken = 0;
          });
    Ok(())
}
pub(crate) fn name(executor: &crate::context::ApplicationExecutor<'_>, text: &str)
                   -> Result<(), Error>
{
    let name = if text.is_empty()
    {
        None
    }
    else
    {
        Some(ThreadName::new(text).map_err(|status| {
                                      Error::Admission { status,
                                                         stage: "thread name" }
                                  })?)
    };
    let scheduler = SCHEDULER.lock();
    let thread = current_row(&scheduler, executor)?;
    *thread.core.name.lock() = name;
    Ok(())
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum RuntimeAdmissionError
{
    Closed
}
impl RuntimeAdmissionError
{
    pub(crate) fn status(&self) -> Status
    {
        mac_abi::OPERATION_CANCELLED
    }
}
impl core::fmt::Display for RuntimeAdmissionError
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        out.write_str("Process runtime admission is closed; prepared key/author ownership was not \
                       published")
    }
}
/// No allocation, callback or owning disposal may run while this turn lives.
/// Declaration order releases SCHEDULER before restoring local interrupts.
pub(crate) struct RuntimeTurn
{
    _scheduler:  spinning_top::SpinlockGuard<'static, Scheduler>,
    _interrupts: super::super::interrupts::InterruptMask
}
pub(crate) fn runtime_turn(admission: &crate::toolbox::native_threads::RuntimeAdmission)
                           -> Result<RuntimeTurn, RuntimeAdmissionError>
{
    let interrupts = super::super::interrupts::mask();
    let scheduler = SCHEDULER.lock();
    if !admission.accepting()
    {
        return Err(RuntimeAdmissionError::Closed);
    }
    Ok(RuntimeTurn { _scheduler:  scheduler,
                     _interrupts: interrupts })
}
