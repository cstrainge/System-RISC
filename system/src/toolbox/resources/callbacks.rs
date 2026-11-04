//! Process callbacks own their payload and original definition execution pin.
//! The canonical Process/callback THRD owns Queue; jobs never own that THRD or
//! Queue. Author execution, copies, decoding, wakeups and Drop run outside gates.
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::{ACCESS_DENIED, BAD_ARGUMENT, NO_MEMORY, SERVICE_BUSY, Status};
use mac_object_table::shared::InvocationGate as _;
use spinning_top::Spinlock;

use crate::{context::ThreadId,
            machine::{scheduler::callbacks::CallbackExecution, traps::user::OwnedBytes},
            toolbox::handles::InvocationGate};

#[path = "callbacks/packet.rs"]
pub(crate) mod packet;
use packet::{Packet, Reply, Request};
#[path = "callbacks/continuation.rs"]
mod continuation;
pub(crate) use continuation::{Continuation, dispatch_user};
#[path = "callbacks/direct.rs"]
mod direct;
pub(crate) use direct::dispatch_supervisor;
#[path = "callbacks/prepared.rs"]
mod prepared;
pub(crate) use prepared::{PreparedControlDisposal, PreparedWindowCallback};

#[path = "callbacks/return_loan.rs"]
mod return_loan;
use return_loan::ReturnStorage;
pub(crate) use return_loan::{ReturnLoan, checkout_prepaid_return, returned_prepaid};

/// A coalesced ready hint, never a callback result or object authority. The
/// System manager preadmits one notice and examines only its real work list.
pub(crate) struct Notice
{
    ready:  AtomicBool,
    parent: Option<Arc<Notice>>
}
impl Notice
{
    pub(crate) fn new() -> Result<Arc<Self>, Status>
    {
        Arc::try_new(Self { ready:  AtomicBool::new(false),
                            parent: None }).map_err(|_| NO_MEMORY)
    }

    /// A job-local ready hint linked to the manager's one common observer.
    /// Depth is exactly one: no recursive publication or ownership chain.
    pub(crate) fn linked(parent: Arc<Notice>) -> Result<Arc<Self>, Status>
    {
        if parent.parent.is_some()
        {
            return Err(BAD_ARGUMENT);
        }
        Arc::try_new(Self { ready:  AtomicBool::new(false),
                            parent: Some(parent) }).map_err(|_| NO_MEMORY)
    }

    /// Pure atomic observation, including the scheduler's park-commit turn.
    pub(crate) fn ready(&self) -> bool
    {
        self.ready.load(Ordering::Acquire)
    }

    pub(crate) fn take(&self) -> bool
    {
        self.ready.swap(false, Ordering::AcqRel)
    }

    /// Publish only a real completion/work-change edge. This atomic hint and
    /// allocation-free primary wake are valid under a manager's short work-list
    /// exclusion. Callback RETURN/stopped notifications run after their guards.
    pub(crate) fn request(&self)
    {
        self.ready.store(true, Ordering::Release);
        if let Some(parent) = &self.parent
        {
            parent.ready.store(true, Ordering::Release);
        }
        crate::machine::scheduler::wake::request();
    }
}
#[derive(Clone)]
pub(crate) enum CompletionTarget
{
    Thread(ThreadId),
    System(Arc<Notice>),
    ThreadAndSystem
    {
        thread: ThreadId,
        notice: Arc<Notice>
    }
}
impl From<ThreadId> for CompletionTarget
{
    fn from(thread: ThreadId) -> Self
    {
        Self::Thread(thread)
    }
}
impl CompletionTarget
{
    fn notify(&self)
    {
        match self
        {
            Self::Thread(thread) => crate::machine::scheduler::callbacks::wake_waiter(*thread),
            Self::System(notice) => notice.request(),
            Self::ThreadAndSystem { thread,
                                    notice } =>
            {
                notice.request();
                crate::machine::scheduler::callbacks::wake_waiter(*thread);
            }
        }
    }
}

pub(crate) struct Queue
{
    state: Spinlock<QueueState>,
    ready: AtomicBool
}
struct QueueState
{
    first:   Option<Arc<Job>>,
    last:    Option<Arc<Job>>,
    running: Option<Arc<Job>>,
    stopped: bool
}
impl Queue
{
    pub(crate) fn new() -> Result<Arc<Self>, Status>
    {
        Arc::try_new(Self { state: Spinlock::new(QueueState { first:   None,
                                                              last:    None,
                                                              running: None,
                                                              stopped: false }),
                            ready: AtomicBool::new(false) }).map_err(|_| NO_MEMORY)
    }

    /// Read under the scheduler's park-commit turn. Enqueue publishes readiness
    /// before its guard-free canonical wake, closing enqueue-before-park.
    pub(crate) fn ready(&self) -> bool
    {
        self.ready.load(Ordering::Acquire)
    }
}
impl Drop for Queue
{
    fn drop(&mut self)
    {
        // The canonical role owns Queue through real THRD quiescence. Normal
        // retirement has already called stopped(); preparation failure has no
        // jobs. Still unlink iteratively so field Drop never recursively walks
        // an arbitrarily long intrusive chain.
        let state = self.state.get_mut();
        let running = state.running.take();
        if let Some(job) = running
        {
            stop_job(job);
        }
        while let Some(job) = state.first.take()
        {
            state.first = InvocationGate::run(|| job.state.lock().next.take());
            stop_job(job);
        }
        state.last = None;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase
{
    New,
    Queued,
    Delivering,
    Running,
    Complete,
    Consumed,
    Cancelled,
    Stopped
}
/// Original immutable family readers/exclusion retained by canonical callback
/// work. Implementations contain no Future, Pending, Queue or Context backedge.
pub(crate) trait CallbackCustody: Send + Sync
{
}

struct Job
{
    identity:   PanicIdentity,
    completion: CompletionTarget,
    complete:   AtomicBool,
    state:      Spinlock<JobState>,
    // LAST: packet/reply/invocation readers in state must be destroyed first.
    custody:    Option<Arc<dyn CallbackCustody>>
}
struct JobState
{
    phase:          Phase,
    next:           Option<Arc<Job>>,
    abandoned:      bool,
    packet:         Option<Arc<Packet>>,
    invocation:     Option<super::Invocation>,
    reply:          Option<Reply>,
    return_storage: ReturnStorage
}

pub(crate) struct Pending
{
    job: Arc<Job>
}
impl Pending
{
    pub(crate) fn ready(&self) -> bool
    {
        self.job.complete.load(Ordering::Acquire)
    }

    /// Actual validated broker RETURN only. This read neither consumes Reply
    /// nor proves object teardown/ACK; managers use it to avoid false self-wait
    /// refusal after author work already ended successfully.
    pub(crate) fn returned_result(&self) -> Option<i64>
    {
        InvocationGate::run(|| {
            let state = self.job.state.lock();
            if state.phase == Phase::Complete
            {
                state.reply.as_ref().map(|reply| reply.result)
            }
            else
            {
                None
            }
        })
    }

    pub(crate) fn poll(&self) -> Result<Option<Reply>, Status>
    {
        InvocationGate::run(|| {
            let mut state = self.job.state.lock();
            match state.phase
            {
                Phase::Complete =>
                {
                    let reply = state.reply.take().ok_or(BAD_ARGUMENT)?;
                    state.phase = Phase::Consumed;
                    Ok(Some(reply))
                }
                Phase::Stopped => Err(SERVICE_BUSY),
                Phase::Consumed => Err(BAD_ARGUMENT),
                _ => Ok(None)
            }
        })
    }
}
impl Drop for Pending
{
    fn drop(&mut self)
    {
        let retired = InvocationGate::run(|| {
            let mut state = self.job.state.lock();
            state.abandoned = true;
            let (invocation, packet) = if state.phase == Phase::Queued
            {
                state.phase = Phase::Cancelled;
                (state.invocation.take(), state.packet.take())
            }
            else
            {
                (None, None)
            };
            (state.reply.take(), invocation, packet)
        });
        drop(retired);
        // A queued/running job remains in the original queue until real RETURN
        // or the scheduler's stopped callback. Dropping a waiter is not an ACK.
    }
}

/// Admission identity is retained until the service's actual status boundary.
/// Variants name executed stages, never infer a cause from a numeric Busy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EnqueueError
{
    Definition(Status),
    EntryKind,
    DefinitionState,
    Source,
    OpaqueOwner,
    Worker(Status),
    WorkerBinding,
    SelfQueue,
    Packet(Status),
    Storage(Status),
    Queue(Status)
}
impl EnqueueError
{
    pub(crate) fn status(self) -> Status
    {
        match self
        {
            Self::Definition(s)
            | Self::Worker(s)
            | Self::Packet(s)
            | Self::Storage(s)
            | Self::Queue(s) => s,
            Self::EntryKind | Self::DefinitionState => BAD_ARGUMENT,
            Self::Source | Self::OpaqueOwner | Self::WorkerBinding => ACCESS_DENIED,
            Self::SelfQueue => SERVICE_BUSY
        }
    }
}

/// Called only after a manager has detached an owned typed operation. The real
/// caller ThreadId is captured by the outer service, never read from user wire.
pub(crate) fn enqueue(reference: super::DefinitionRef, request: Request,
                      completion: impl Into<CompletionTarget>)
                      -> Result<Pending, Status>
{
    enqueue_with(reference, request, completion.into(), None, None, Ok).map_err(EnqueueError::status)
}

/// A real object producer lends its original storage authority. Code ownership
/// authorizes execution; it does not pay another object's request/response data.
/// This shares the same packet validation, FIFO commit and callback transport.
pub(crate) fn enqueue_for_object(reference: super::DefinitionRef, request: Request,
                                 completion: CompletionTarget,
                                 storage: &crate::machine::scheduler::accounting::AllocationAuthority)
                                 -> Result<Pending, EnqueueError>
{
    enqueue_with(reference, request, completion, Some(storage), None, Ok)
}

/// Shared observer admitted before the same object-budget FIFO publication.
pub(crate) fn enqueue_shared_for_object(reference: super::DefinitionRef, request: Request,
                                        completion: CompletionTarget,
                                        storage:&crate::machine::scheduler::accounting::AllocationAuthority,
                                        custody: Option<Arc<dyn CallbackCustody>>)
                                        -> Result<Arc<Pending>, EnqueueError>
{
    enqueue_with(reference, request, completion, Some(storage), custody, |pending| {
        Arc::try_new(pending).map_err(|_| NO_MEMORY)
    })
}

fn enqueue_with<T>(reference: super::DefinitionRef, request: Request,
                   completion: CompletionTarget,
                   storage: Option<&crate::machine::scheduler::accounting::AllocationAuthority>,
                   custody: Option<Arc<dyn CallbackCustody>>,
                   wrap: impl FnOnce(Pending) -> Result<T, Status>)
                   -> Result<T, EnqueueError>
{
    let (invocation, entry) = super::pin(reference, request.kind(), request.id(), false).map_err(EnqueueError::Definition)?;
    let super::Entry::User(entry) = entry
    else
    {
        return Err(EnqueueError::EntryKind);
    };
    let definition = invocation.definition.as_ref().ok_or(EnqueueError::DefinitionState)?;
    let super::SourceBinding::Image(source) = &definition.source
    else
    {
        return Err(EnqueueError::Source);
    };
    if request.opaque_owner().is_some_and(|(owner, image)| {
                                 source.owner().owner() != owner || source.binding().id() != image
                             })
    {
        return Err(EnqueueError::OpaqueOwner);
    }
    let worker =
        crate::machine::scheduler::callbacks::prepare(source).map_err(EnqueueError::Worker)?;
    if worker.owner().owner() != definition.owner.owner()
       || worker.image_id() != source.binding().id()
    {
        return Err(EnqueueError::WorkerBinding);
    }
    if crate::machine::scheduler::callbacks::authenticate_current().is_ok_and(|current| {
                                                                       Arc::ptr_eq(current.queue(),
                                                                                   worker.queue())
                                                                   })
    {
        // The sole callback thread cannot wait for work that only it can run.
        // Ordinary nested font/theme/surface services do not enqueue work.
        return Err(EnqueueError::SelfQueue);
    }
    let identity = PanicIdentity { reference,
                                   kind: definition.identity.kind,
                                   id: definition.identity.id,
                                   owner: definition.owner.owner(),
                                   image_id: source.binding().id() };
    let allocation = storage.map_or_else(|| definition.source.allocation_target(), |account| {
        crate::allocator::Target::Owner { account: account.clone(), class: mac_kernel_model::policy::MemoryClass::Kernel }
    });
    let (job, pending) = crate::allocator::with_target(allocation, || {
        let packet = Arc::try_new(request.prepare(entry).map_err(EnqueueError::Packet)?).map_err(|_| EnqueueError::Storage(NO_MEMORY))?;
        let job = Arc::try_new(Job { identity,
                                     completion,
                                     complete: AtomicBool::new(false),
                                     state: Spinlock::new(JobState { phase:          Phase::New,
                                                                     next:           None,
                                                                     abandoned:      false,
                                                                     packet:         Some(packet),
                                                                     invocation:
                                                                         Some(invocation),
                                                                     reply:          None,
                                                                     return_storage:
                                                                         ReturnStorage::Ordinary }),
                                     custody }).map_err(|_| EnqueueError::Storage(NO_MEMORY))?;
        let pending = wrap(Pending { job: job.clone() }).map_err(EnqueueError::Storage)?;
        Ok::<_, EnqueueError>((job, pending))
    })?;
    let queue = worker.queue();
    enqueue_job(queue, &job, &mut None).map_err(EnqueueError::Queue)?;
    crate::machine::scheduler::callbacks::wake(&worker);
    Ok(pending)
}

/// Canonical FIFO commit shared by ordinary and preadmitted work. An optional
/// freshly admitted pin moves only after every refusal check, before linking.
fn enqueue_job(queue: &Arc<Queue>, job: &Arc<Job>, invocation: &mut Option<super::Invocation>)
               -> Result<(), Status>
{
    InvocationGate::run(|| {
        let mut state = queue.state.lock();
        if state.stopped
        {
            return Err(SERVICE_BUSY);
        }
        let mut cell = job.state.lock();
        if cell.phase != Phase::New
           || (cell.invocation.is_some() && invocation.is_some())
           || (cell.invocation.is_none() && invocation.is_none())
        {
            return Err(BAD_ARGUMENT);
        }
        if invocation.is_some()
        {
            cell.invocation = invocation.take();
        }
        // No allocation or fallible work remains after the execution pin move.
        cell.phase = Phase::Queued;
        if let Some(last) = &state.last
        {
            last.state.lock().next = Some(job.clone());
        }
        else
        {
            state.first = Some(job.clone());
        }
        state.last = Some(job.clone());
        queue.ready.store(true, Ordering::Release);
        Ok(())
    })
}

fn authentic(queue: &Arc<Queue>, execution: &CallbackExecution) -> Result<(), Status>
{
    if Arc::ptr_eq(queue, execution.queue()) { Ok(()) } else { Err(ACCESS_DENIED) }
}

#[derive(Clone, Copy)]
pub(crate) struct PanicIdentity
{
    pub(crate) reference: super::DefinitionRef,
    pub(crate) kind:      [u8; 4],
    pub(crate) id:        i16,
    pub(crate) owner:     mac_object_table::domain::DomainId,
    pub(crate) image_id:  u64
}
pub(crate) fn panic_identity(queue: &Arc<Queue>, execution: &CallbackExecution)
                             -> Result<PanicIdentity, Status>
{
    authentic(queue, execution)?;
    InvocationGate::run(|| {
        let state = queue.state.lock();
        if state.stopped
        {
            return Err(SERVICE_BUSY);
        }
        let job = state.running.as_ref().ok_or(ACCESS_DENIED)?;
        if job.state.lock().phase != Phase::Running
        {
            return Err(ACCESS_DENIED);
        }
        if job.state.lock().invocation.is_none()
        {
            return Err(ACCESS_DENIED);
        }
        Ok(job.identity)
    })
}

pub(crate) enum Next
{
    Wait,
    Required(usize),
    Work(Delivery)
}

/// The outer transport must complete user copyout before committing delivery.
/// A refused copy drops this lease and leaves the same original job available.
pub(crate) struct Delivery
{
    packet:    Arc<Packet>,
    queue:     Arc<Queue>,
    committed: bool,
    // LAST even if another genuine owner already detached JobState readers.
    job:       Arc<Job>
}
impl Delivery
{
    pub(crate) fn bytes(&self) -> &[u8]
    {
        self.packet.bytes()
    }

    pub(crate) fn commit(mut self) -> Result<(), Status>
    {
        InvocationGate::run(|| {
            let state = self.queue.state.lock();
            if state.stopped || !state.running.as_ref().is_some_and(|j| Arc::ptr_eq(j, &self.job))
            {
                return Err(SERVICE_BUSY);
            }
            let mut job = self.job.state.lock();
            if job.phase != Phase::Delivering
            {
                return Err(BAD_ARGUMENT);
            }
            job.phase = Phase::Running;
            Ok(())
        })?;
        self.committed = true;
        Ok(())
    }
}
impl Drop for Delivery
{
    fn drop(&mut self)
    {
        if self.committed
        {
            return;
        }
        InvocationGate::run(|| {
            let state = self.queue.state.lock();
            let mut job = self.job.state.lock();
            if !state.stopped && job.phase == Phase::Delivering
            {
                // Retain running's exact job for the next delivery attempt.
                job.phase = if job.abandoned { Phase::Cancelled } else { Phase::Queued };
                self.queue.ready.store(true, Ordering::Release);
            }
        });
    }
}

pub(crate) fn next(queue: &Arc<Queue>, execution: &CallbackExecution, capacity: usize)
                   -> Result<Next, Status>
{
    authentic(queue, execution)?;
    enum Step
    {
        Done(Next),
        Discard(Arc<Job>)
    }
    loop
    {
        let step = InvocationGate::run(|| {
            let mut state = queue.state.lock();
            if state.stopped
            {
                return Err(SERVICE_BUSY);
            }
            if let Some(job) = &state.running
            {
                let mut cell = job.state.lock();
                if cell.phase == Phase::Cancelled
                {
                    drop(cell);
                    return Ok(Step::Discard(state.running.take().ok_or(BAD_ARGUMENT)?));
                }
                if cell.phase != Phase::Queued
                {
                    return Err(SERVICE_BUSY);
                }
                let packet = cell.packet.as_ref().ok_or(BAD_ARGUMENT)?.clone();
                if capacity < packet.bytes().len()
                {
                    return Ok(Step::Done(Next::Required(packet.bytes().len())));
                }
                cell.phase = Phase::Delivering;
                return Ok(Step::Done(Next::Work(Delivery { queue: queue.clone(),
                                                           job: job.clone(),
                                                           packet,
                                                           committed: false })));
            }
            let Some(first) = state.first.as_ref().cloned()
            else
            {
                queue.ready.store(false, Ordering::Release);
                return Ok(Step::Done(Next::Wait));
            };
            let mut cell = first.state.lock();
            if !matches!(cell.phase, Phase::Queued | Phase::Cancelled)
            {
                return Err(BAD_ARGUMENT);
            }
            let packet = if cell.phase == Phase::Queued
            {
                Some(cell.packet.as_ref().ok_or(BAD_ARGUMENT)?.clone())
            }
            else
            {
                None
            };
            if let Some(packet) = &packet
               && capacity < packet.bytes().len()
            {
                return Ok(Step::Done(Next::Required(packet.bytes().len())));
            }
            let job = state.first.take().ok_or(BAD_ARGUMENT)?;
            state.first = cell.next.take();
            if state.first.is_none()
            {
                state.last = None;
            }
            if cell.phase == Phase::Cancelled
            {
                return Ok(Step::Discard(job));
            }
            let packet = packet.ok_or(BAD_ARGUMENT)?;
            cell.phase = Phase::Delivering;
            drop(cell);
            state.running = Some(job.clone());
            queue.ready.store(state.first.is_some(), Ordering::Release);
            Ok(Step::Done(Next::Work(Delivery { queue: queue.clone(),
                                                job,
                                                packet,
                                                committed: false })))
        })?;
        match step
        {
            Step::Done(next) => return Ok(next),
            Step::Discard(job) => drop(job)
        }
    }
}

/// Input has already been copied into owned supervisor storage. Kernel-held
/// ranges and the original typed payload determine what can become a reply.
pub(crate) fn returned(queue: &Arc<Queue>, execution: &CallbackExecution, bytes: OwnedBytes,
                       result: i64)
                       -> Result<(), (Status, OwnedBytes)>
{
    returned_for(queue, execution, bytes, result, None)
}
fn returned_for(queue: &Arc<Queue>, execution: &CallbackExecution, bytes: OwnedBytes, result: i64,
                expected: Option<&Arc<Job>>)
                -> Result<(), (Status, OwnedBytes)>
{
    if let Err(status) = authentic(queue, execution)
    {
        return Err((status, bytes));
    }
    let running = InvocationGate::run(|| {
        let state = queue.state.lock();
        let job = state.running.as_ref().ok_or(BAD_ARGUMENT)?;
        if expected.is_some_and(|expected| !Arc::ptr_eq(expected, job))
        {
            return Err(ACCESS_DENIED);
        }

        let cell = job.state.lock();
        if state.stopped || cell.phase != Phase::Running
        {
            return Err(SERVICE_BUSY);
        }
        let packet = cell.packet.as_ref().ok_or(BAD_ARGUMENT)?.clone();
        Ok((job.clone(), packet))
    });
    let (job, packet) = match running
    {
        Ok(value) => value,
        Err(status) => return Err((status, bytes))
    };
    let mut reply = packet.reply(bytes, result)?;
    // Same canonical keeper follows owned returned bytes, even if the original
    // future/Pending disappears before decoding or final publication.
    reply.custody = job.custody.clone();
    let job_ref = &job;
    let retired = InvocationGate::run(move || {
        let mut state = queue.state.lock();
        if state.stopped || !state.running.as_ref().is_some_and(|j| Arc::ptr_eq(j, job_ref))
        {
            return Err((SERVICE_BUSY, reply));
        }
        let mut cell = job_ref.state.lock();
        if cell.phase != Phase::Running
        {
            return Err((BAD_ARGUMENT, reply));
        }
        cell.reply = Some(reply);
        cell.phase = Phase::Complete;
        job_ref.complete.store(true, Ordering::Release);
        Ok((state.running.take(), cell.invocation.take(), cell.packet.take()))
    });
    match retired
    {
        Ok(retired) =>
        {
            drop(retired);
            job.completion.notify();
            Ok(())
        }
        Err((status, reply)) => Err((status, reply.into_bytes()))
    }
}

/// Called by the scheduler only after the canonical callback THRD cannot run
/// or resume. No copied user state or semantic close can provide this ACK.
pub(crate) fn stopped(queue: &Arc<Queue>)
{
    let running = InvocationGate::run(|| {
        let mut state = queue.state.lock();
        state.stopped = true;
        queue.ready.store(true, Ordering::Release);
        state.running.take()
    });
    if let Some(job) = running
    {
        stop_job(job);
    }
    loop
    {
        let job = InvocationGate::run(|| {
            let mut state = queue.state.lock();
            let job = state.first.take()?;
            state.first = job.state.lock().next.take();
            if state.first.is_none()
            {
                state.last = None;
            }
            Some(job)
        });
        let Some(job) = job
        else
        {
            break;
        };
        stop_job(job);
    }
}

fn stop_job(job: Arc<Job>)
{
    let retired = InvocationGate::run(|| {
        let mut state = job.state.lock();
        state.phase = Phase::Stopped;
        job.complete.store(true, Ordering::Release);
        (state.reply.take(),
         state.invocation.take(),
         state.packet.take(),
         core::mem::replace(&mut state.return_storage, ReturnStorage::Ordinary))
    });
    drop(retired);
    job.completion.notify();
    drop(job);
}
