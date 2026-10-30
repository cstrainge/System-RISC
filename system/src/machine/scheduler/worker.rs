//! Module workers share the scheduler's real thread store and CPU switch path.
//! Preparation owns unpublished backing; published THRD roots retain callbacks
//! until cooperative return, explicit destruction and quiescent physical reap.
use mac_kernel_model::policy::MemoryClass;

use super::*;
use crate::machine::{emergency, interrupts, traps};
// Foreign destruction runs only at the scheduler's guard-free maintenance
// boundary; an application launch may independently reap process backing.
static WORKER_PENDING: AtomicBool = AtomicBool::new(false);
pub(super) fn context_released()
{
    if WORKER_REAP_COUNT.load(Ordering::Acquire) != 0
    {
        WORKER_PENDING.store(true, Ordering::Release);
    }
}

/// Called once at the actual first service fault, while SCHEDULER still owns
/// this exact THRD. Permanent retention never polls the ordinary worker reaper.
pub(super) fn service_quarantined(thread: &Thread)
{
    let Some(worker) = thread.worker.as_ref()
    else
    {
        return;
    };
    let mut state = worker.get();
    state.cancelled = true;
    if state.returned
    {
        if WORKER_REAP_COUNT.try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                                count.checked_sub(1)
                            })
                            .is_err()
        {
            // Retain the original permanent fault; do not wrap accounting or
            // panic while attempting cleanup of already uncertain ownership.
            thread.core
                  .cleanup_refusal
                  .lock()
                  .replace(ReapState::NativeService(mac_abi::CORRUPT_DATA));
        }
    }
    worker.set(state);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReapState
{
    Refused(mac_abi::Status),
    NativeService(mac_abi::Status),
    Running
    {
        hart: usize
    },
    Readers
    {
        core:    usize,
        context: usize
    }
}

#[derive(Clone, Copy)]
pub(super) struct ThreadName
{
    bytes:  [u8; mac_module::MAX_NAME_BYTES],
    length: usize
}
impl ThreadName
{
    pub(super) fn new(name: &str) -> Result<Self, mac_abi::Status>
    {
        if name.is_empty()
           || name.len() > mac_module::MAX_NAME_BYTES
           || name.chars().any(char::is_control)
        {
            return Err(mac_abi::BAD_ARGUMENT);
        }
        let mut result = Self { bytes:  [0; mac_module::MAX_NAME_BYTES],
                                length: name.len() };
        result.bytes[..result.length].copy_from_slice(name.as_bytes());
        Ok(result)
    }

    pub(super) fn text(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.length]).unwrap_or("Invalid admitted thread name")
    }
}

#[derive(Clone, Copy)]
pub(super) struct WorkerState
{
    pub(super) phase:       u32,
    pub(super) cancelled:   bool,
    pub(super) wake:        bool,
    pub(super) dispose:     bool,
    pub(super) started:     bool,
    // Set at the scheduler's real restore commitment; unlike started this
    // survives preemption before the trampoline reaches its first check.
    resume_committed:       bool,
    pub(super) returned:    bool,
    // PARK has committed but its return has not yet been selected for restore.
    // Other harts record cancellation; only a frame-owning handoff writes a0.
    park_return:            bool,
    pub(super) exit_status: mac_abi::Status
}
pub(crate) struct WorkerStatus
{
    pub(crate) state:       u32,
    pub(crate) exit_status: mac_abi::Status,
    pub(crate) cancelled:   bool
}
pub(crate) struct PreparedWorker
{
    owner:      OwnerRef,
    core:       mac_object_table::retained::Read<ThreadCore>,
    allocation: accounting::AllocationAuthority,
    subject:    crate::drivers::jobs::subjects::Subject,
    affinity:   super::ThreadAffinity
}
pub(crate) struct WorkerPrepareFailure
{
    pub(crate) status: mac_abi::Status,
    pub(crate) launch: crate::drivers::WorkerLaunch
}
pub(crate) struct WorkerPublishFailure
{
    pub(crate) status:   mac_abi::Status,
    pub(crate) prepared: PreparedWorker
}
impl PreparedWorker
{
    pub(crate) fn return_launch(self) -> Result<crate::drivers::WorkerLaunch, Self>
    {
        let launch = self.core.launch.lock().take();
        match launch
        {
            Some(launch) => Ok(launch),
            None => Err(self)
        }
    }
}
pub(crate) fn prepare_worker(launch: crate::drivers::WorkerLaunch)
                             -> Result<PreparedWorker, WorkerPrepareFailure>
{
    let affinity = launch.affinity();
    let owner = launch.owner().clone();
    let allocation = launch.allocation().clone();
    let target = crate::allocator::Target::Owner { account: allocation.clone(),
                                                   class:   MemoryClass::Kernel };
    let result = crate::allocator::with_target(target, || {
        if owner.owner().kind() != mac_object_table::domain::DomainKind::Module
           || allocation.owner() != owner.owner()
           || launch.execution().module() != owner.owner().incarnation()
           || !owner.accepting()
        {
            return Err(mac_abi::ACCESS_DENIED);
        }
        let name = ThreadName::new(launch.name())?;
        let charges =
            accounting::ThreadCharges::admit_module(&owner, &allocation).map_err(|error| {
                                                                            error.status()
                                                                        })?;
        let context = Arc::try_new(ThreadContext::new()).map_err(|_| mac_abi::NO_MEMORY)?;
        let authority = crate::toolbox::handles::subject::Authority::Client(owner.clone());
        let target = crate::allocator::Target::Owner { account: allocation.clone(),
                                                       class:   MemoryClass::Kernel };
        let subject = crate::drivers::jobs::subjects::resident()?.admit(&authority, target)?;
        let (stack, cpu, _) = charges.allocate(&owner, |top| {
                                         let mut frame = TrapFrame::default();
                                         frame.pc = worker_entry as *const () as usize;
                                         frame.status = traps::INITIAL_STATUS;
                                         frame.stack_pointer = top;
                                         frame
                                     })
                                     .map_err(|error| error.status())?;
        let top = stack.top();
        let stack = super::stack::StackBacking::prepare(&owner, stack)?;
        let backing = ThreadBacking { cpu,
                                      stack,
                                      image: None };
        let core = ThreadCore { address: Mutex::new(None),
                                backing: Mutex::new(Some(backing)),
                                launch: Mutex::new(None),
                                context,
                                release: Mutex::new(None),
                                binding: launch.execution().binding(),
                                top,
                                cleanup_refusal: Mutex::new(None),
                                service_quarantine: spin::Once::new(),
                                name: Mutex::new(Some(name)),
                                application: None,
                                pending_join: Mutex::new(None) };
        let core = Arc::try_new(core).map_err(|_| mac_abi::NO_MEMORY)?;
        Ok((core, subject))
    });
    match result
    {
        Ok((core, subject)) =>
        {
            *core.launch.lock() = Some(launch);
            Ok(PreparedWorker { owner,
                                allocation,
                                subject,
                                affinity,
                                core: mac_object_table::retained::Read::new(core,
                                                                            context_released) })
        }
        Err(status) =>
        {
            Err(WorkerPrepareFailure { status,
                                       launch })
        }
    }
}
pub(crate) fn publish_worker(prepared: PreparedWorker) -> Result<u64, WorkerPublishFailure>
{
    let result = interrupts::with_masked(|| {
        let mut scheduler = SCHEDULER.lock();
        let slot = crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
            scheduler.prepare()
        })?;
        if prepared.core.launch.lock().is_none()
        {
            return Err(mac_abi::BAD_ABI);
        }
        let target = crate::allocator::Target::Owner { account: prepared.allocation.clone(),
                                                       class:   MemoryClass::Kernel };
        let publication = crate::allocator::with_target(target, || {
            let kind = crate::toolbox::handles::KindCode(*b"THRD");
            let root = mac_object_table::scope::Reservation::try_new(&prepared.owner, kind)
                .map_err(lifetime::pool_status)?;
            let scope = root.lease();
            let store = scheduler.records.as_mut().ok_or(mac_abi::BAD_ABI)?;
            let record = |id| {
                let worker = WorkerState { phase:            mac_module::abi::worker::PREPARED,
                                           cancelled:        false,
                                           wake:             false,
                                           dispose:          false,
                                           started:          false,
                                           resume_committed: false,
                                           returned:         false,
                                           park_return:      false,
                                           exit_status:      mac_abi::OK };
                let mut state = RunState::new();
                state.affinity = prepared.affinity;
                Record::Scheduled(Thread { id,
                                           core: prepared.core.share(),
                                           scope: Some(scope),
                                           worker: Some(Cell::new(worker)),
                                           state: Cell::new(state) })
            };
            store.insert_root_with_receipt(root, record).map_err(lifetime::pool_status)
        })?;
        let (id, receipt) = publication.into_parts();
        // No fallible operation remains after this unique ownership handoff.
        {
            let mut launch = prepared.core.launch.lock();
            if let Some(launch) = launch.as_mut()
            {
                prepared.core.context.assign_module(prepared.owner.clone(),
                                                    id,
                                                    launch.execution(),
                                                    &prepared.allocation,
                                                    prepared.subject.clone());
                launch.accept();
            }
        }
        if slot == scheduler.threads.len()
        {
            scheduler.threads.push(Some(receipt));
        }
        else
        {
            scheduler.threads[slot] = Some(receipt);
        }
        Ok(id)
    });
    match result
    {
        Ok(id) =>
        {
            let info = {
                let backing = prepared.core.backing.lock();
                backing.as_ref().map(|backing| {
                                    (backing.stack.bytes(),
                                     backing.stack.backing_bytes(),
                                     backing.cpu.bytes(),
                                     backing.cpu.backing_bytes())
                                })
            };
            let name = *prepared.core.name.lock();
            if let (Some(name), Some((stack_bytes, stack_backing, cpu_bytes, cpu_backing))) =
                (name, info)
            {
                serial!("MODULE_WORKER_THREAD module={} thread={} name={} stack_bytes={} \
                         stack_backing_bytes={} cpu_state_bytes={} cpu_backing_bytes={}",
                        prepared.owner.owner().incarnation(),
                        id,
                        name.text(),
                        stack_bytes,
                        stack_backing,
                        cpu_bytes,
                        cpu_backing);
            }
            Ok(id)
        }
        Err(status) =>
        {
            Err(WorkerPublishFailure { status,
                                       prepared })
        }
    }
}
fn checked_worker<'a>(scheduler: &'a Scheduler, owner: &OwnerRef, id: u64)
                      -> Result<&'a Thread, mac_abi::Status>
{
    let thread = scheduler.records
                          .as_ref()
                          .and_then(|s| s.get(id))
                          .and_then(Record::scheduled)
                          .ok_or(mac_abi::NOT_FOUND)?;
    if thread.worker.is_none() || thread.scope.as_ref().is_none_or(|s| s.subject() != owner.owner())
    {
        return Err(mac_abi::ACCESS_DENIED);
    }
    if let Some(cause) = thread.state.get().service.failure()
    {
        return Err(cause);
    }
    Ok(thread)
}
pub(crate) fn worker_start(owner: &OwnerRef, id: u64, activate: bool)
                           -> Result<(), mac_abi::Status>
{
    interrupts::with_masked(|| {
        let scheduler = SCHEDULER.lock();
        let thread = checked_worker(&scheduler, owner, id)?;
        if !owner.accepting()
        {
            return Err(mac_abi::SERVICE_BUSY);
        }
        let Some(worker) = &thread.worker
        else
        {
            return Err(mac_abi::ACCESS_DENIED);
        };
        let mut state = worker.get();
        if state.phase != mac_module::abi::worker::PREPARED
           && state.phase != mac_module::abi::worker::ARMED
        {
            return Err(mac_abi::SERVICE_BUSY);
        }
        state.phase = if activate
        {
            mac_module::abi::worker::RUNNING
        }
        else
        {
            mac_module::abi::worker::ARMED
        };
        worker.set(state);
        thread.update(|s| s.live = activate);
        Ok(())
    })
}
pub(crate) fn activate_workers(owner: &OwnerRef)
{
    if !owner.accepting()
    {
        return;
    }
    interrupts::with_masked(|| {
        let scheduler = SCHEDULER.lock();
        for (_, thread) in scheduler.iter()
        {
            if thread.scope.as_ref().is_some_and(|s| s.subject() == owner.owner())
            {
                if let Some(worker) = &thread.worker
                {
                    let mut state = worker.get();
                    if state.phase == mac_module::abi::worker::ARMED
                       && !state.cancelled
                       && thread.state.get().service.failure().is_none()
                    {
                        state.phase = mac_module::abi::worker::RUNNING;
                        worker.set(state);
                        thread.update(|s| s.live = true);
                    }
                }
            }
        }
    })
}
pub(crate) fn worker_status(owner: &OwnerRef, id: u64) -> Result<WorkerStatus, mac_abi::Status>
{
    interrupts::with_masked(|| {
        let scheduler = SCHEDULER.lock();
        let thread = checked_worker(&scheduler, owner, id)?;
        let state = thread.worker.as_ref().ok_or(mac_abi::ACCESS_DENIED)?.get();
        Ok(WorkerStatus { state:       state.phase,
                          exit_status: state.exit_status,
                          cancelled:   state.cancelled })
    })
}
pub(super) fn cancel_worker(thread: &Thread, dispose: bool)
{
    if let Some(worker) = &thread.worker
    {
        let mut state = worker.get();
        state.cancelled = true;
        state.dispose |= dispose;
        if let Some(cause) = thread.state.get().service.failure()
        {
            // The owner walk may still record disposal intent, but a faulted
            // body cannot become runnable or acquire a retryable reap receipt.
            state.exit_status = cause;
            worker.set(state);
            return;
        }
        if !state.started && !state.resume_committed
        {
            if !state.returned && thread.state.get().service.failure().is_none()
            {
                WORKER_REAP_COUNT.fetch_add(1, Ordering::AcqRel);
            }
            state.returned = true;
            state.phase = mac_module::abi::worker::FINISHED;
            state.exit_status = mac_abi::OPERATION_CANCELLED;
            thread.update(|s| {
                      s.live = false;
                      s.waiting = None;
                  });
        }
        else if !state.returned
        {
            state.phase = mac_module::abi::worker::RUNNING;
            state.wake = true;
            thread.update(|s| {
                      s.live = true;
                      s.waiting = None;
                      s.woken = 1;
                  });
        }
        worker.set(state);
        WORKER_PENDING.store(true, Ordering::Release);
    }
}
pub(crate) fn worker_cancel(owner: &OwnerRef, id: u64) -> Result<(), mac_abi::Status>
{
    interrupts::with_masked(|| {
        let scheduler = SCHEDULER.lock();
        cancel_worker(checked_worker(&scheduler, owner, id)?, false);
        Ok(())
    })
}
pub(crate) fn worker_dispose(owner: &OwnerRef, id: u64) -> Result<(), mac_abi::Status>
{
    // Disposal is an infallible intent after live/foreign validation; the actual
    // unique removal and any fallible destructor belong to the quiescent reaper.
    interrupts::with_masked(|| {
        let scheduler = SCHEDULER.lock();
        cancel_worker(checked_worker(&scheduler, owner, id)?, true);
        Ok(())
    })
}
pub(crate) fn worker_wake(owner: &OwnerRef, id: u64) -> Result<(), mac_abi::Status>
{
    interrupts::with_masked(|| {
        let scheduler = SCHEDULER.lock();
        let thread = checked_worker(&scheduler, owner, id)?;
        let worker = thread.worker.as_ref().ok_or(mac_abi::ACCESS_DENIED)?;
        let mut state = worker.get();
        if state.returned
        {
            return Ok(());
        }
        state.wake = true;
        if state.started
        {
            state.phase = mac_module::abi::worker::RUNNING;
            thread.update(|s| {
                      s.waiting = None;
                      s.woken = 1;
                  });
        }
        worker.set(state);
        Ok(())
    })
}
pub(crate) fn worker_current() -> Result<(u64, bool), mac_abi::Status>
{
    if in_io()
    {
        return Err(mac_abi::WRONG_THREAD);
    }
    interrupts::with_masked(|| {
        let scheduler = SCHEDULER.lock();
        let thread = scheduler.thread(current()).ok_or(mac_abi::WRONG_THREAD)?;
        if thread.state.get().claimed_by != Some(selected_hart())
        {
            return Err(mac_abi::WRONG_THREAD);
        }
        if !thread.core.context.is_worker_body()
        {
            return Err(mac_abi::WRONG_THREAD);
        }
        let state = thread.worker.as_ref().ok_or(mac_abi::WRONG_THREAD)?.get();
        Ok((thread.id, state.cancelled))
    })
}
pub(crate) fn worker_park(deadline: Option<u64>) -> Result<bool, mac_abi::Status>
{
    let scheduler = SCHEDULER.lock();
    let thread = scheduler.thread(current()).ok_or(mac_abi::WRONG_THREAD)?;
    if let Some(cause) = thread.state.get().service.failure()
    {
        return Err(cause);
    }
    let worker = thread.worker.as_ref().ok_or(mac_abi::WRONG_THREAD)?;
    let mut state = worker.get();
    if state.cancelled
    {
        return Err(mac_abi::OPERATION_CANCELLED);
    }
    if state.wake
    {
        state.wake = false;
        worker.set(state);
        return Ok(false);
    }
    if deadline.is_some_and(|deadline| crate::platform::ticks() >= deadline)
    {
        return Ok(false);
    }
    state.phase = mac_module::abi::worker::PARKED;
    state.park_return = true;
    worker.set(state);
    thread.update(|s| {
              s.waiting = Some(Wait::Worker { deadline });
              s.woken = 0;
          });
    Ok(true)
}
pub(crate) fn worker_exit() -> Result<(), mac_abi::Status>
{
    let scheduler = SCHEDULER.lock();
    let thread = scheduler.thread(current()).ok_or(mac_abi::WRONG_THREAD)?;
    let worker = thread.worker.as_ref().ok_or(mac_abi::WRONG_THREAD)?;
    let mut state = worker.get();
    if let Some(cause) = thread.state.get().service.failure()
    {
        if !state.returned
        {
            return Err(cause);
        }
        // The real body has returned and its launch/Core owners are back in
        // the canonical row before this sole trampoline EXIT. Schedule away,
        // retaining the permanent quarantine rather than returning to its
        // unexpected-EXIT System panic. No reaper or cleanup ACK is issued.
        state.phase = mac_module::abi::worker::FINISHED;
        state.exit_status = cause;
        worker.set(state);
        thread.update(|s| {
                  s.live = false;
                  s.waiting = None;
              });
        return Ok(());
    }
    if !thread.state.get().service.quiescent()
    {
        return Err(mac_abi::SERVICE_BUSY);
    }
    if !state.returned
    {
        return Err(mac_abi::ACCESS_DENIED);
    }
    state.phase = mac_module::abi::worker::FINISHED;
    worker.set(state);
    thread.update(|s| {
              s.live = false;
              s.waiting = None;
          });
    debug_assert!(!thread.state.get().live, "FINISHED worker cannot remain runnable");
    WORKER_PENDING.store(true, Ordering::Release);
    Ok(())
}
fn refuse_entry(thread: &Thread, worker: &Cell<WorkerState>)
{
    let mut state = worker.get();
    if !state.returned && thread.state.get().service.failure().is_none()
    {
        WORKER_REAP_COUNT.fetch_add(1, Ordering::AcqRel);
    }
    state.returned = true;
    // This trampoline has already committed execution. Even a typed entry
    // refusal keeps its real stack runnable until the actual private EXIT.
    state.exit_status = thread.state.get().service.failure().unwrap_or(mac_abi::BAD_ABI);
    worker.set(state);
    thread.update(|state| {
              state.waiting = None;
              state.woken = 0;
          });
    WORKER_PENDING.store(true, Ordering::Release);
}
extern "C" fn worker_entry() -> !
{
    let work = interrupts::with_masked(|| {
        let scheduler = SCHEDULER.lock();
        let slot = current();
        let Some(thread) = scheduler.thread(slot)
        else
        {
            emergency::enter(crate::emergency::Kind::SystemPanic,
                             format_args!("Worker trampoline has no retained scheduler row: \
                                           slot {slot}."));
        };
        if thread.state.get().claimed_by != Some(selected_hart())
        {
            claim_lost(selected_hart(), slot, "worker entry does not own its selected THRD");
        }
        let Some(worker) = thread.worker.as_ref()
        else
        {
            emergency::enter(crate::emergency::Kind::SystemPanic,
                             format_args!("Worker trampoline selected a nonworker row: thread \
                                           {}.",
                                          thread.id));
        };
        let mut state = worker.get();
        if state.cancelled
        {
            // Selection can precede cancellation before the first instruction.
            // Leave the accepted capture/code pin in the row for quiescent Drop.
            // Keep RUNNING/live through any pre-EXIT timer preemption, just as
            // a normally returned body does; only private EXIT finishes it.
            if !state.returned && thread.state.get().service.failure().is_none()
            {
                WORKER_REAP_COUNT.fetch_add(1, Ordering::AcqRel);
            }
            state.returned = true;
            state.exit_status =
                thread.state.get().service.failure().unwrap_or(mac_abi::OPERATION_CANCELLED);
            worker.set(state);
            return None;
        }
        let Some(scope) = thread.scope.clone()
        else
        {
            refuse_entry(thread, worker);
            return None;
        };
        // Validate the scope before taking the accepted capture. A refusal
        // leaves its code pin and destruction capability in the actual row.
        let Some(launch) = thread.core.launch.lock().take()
        else
        {
            refuse_entry(thread, worker);
            return None;
        };
        let mut state = worker.get();
        state.started = true;
        worker.set(state);
        Some((thread.id, thread.core.clone(), scope, launch))
    });
    if let Some((id, core, scope, mut launch)) = work
    {
        let target =
            crate::allocator::Target::Owner { account: launch.allocation().clone(),
                                              class:   MemoryClass::Kernel }.in_scope(&scope);
        let result = match target
        {
            Ok(target) =>
            {
                match core.context.install_worker_allocation(target)
                {
                    Ok(()) => launch.run(),
                    Err(status) => status
                }
            }
            Err(error) => crate::allocation::account_status(&error)
        };
        // Return the code-dependent payload before abandoning this stack. The
        // reaper retains its code pin until both physical backing and callbacks leave.
        interrupts::with_masked(|| {
            let scheduler = SCHEDULER.lock();
            let current = current();
            let thread = scheduler.thread(current);
            let valid = thread.is_some_and(|thread| {
                                  thread.id == id
                                  && Arc::ptr_eq(&thread.core, &core)
                                  && thread.worker.is_some()
                              });
            if !valid
            {
                // No normal disposal/walk removes a live worker. Preserve the
                // local code/stack pin and terminate structural corruption;
                // dropping the last core here could free this active stack.
                emergency::enter(crate::emergency::Kind::SystemPanic,
                                 format_args!("Worker scheduler row disappeared before EXIT: \
                                               module {}, thread {}, slot {}.",
                                              launch.owner().owner().incarnation(),
                                              id,
                                              current));
            }
            let failure = thread.and_then(|thread| thread.state.get().service.failure());
            if let Some(worker) = thread.and_then(|thread| thread.worker.as_ref())
            {
                let mut state = worker.get();
                if !state.returned && failure.is_none()
                {
                    WORKER_REAP_COUNT.fetch_add(1, Ordering::AcqRel);
                }
                state.returned = true;
                state.exit_status = failure.unwrap_or(result);
                worker.set(state);
            }
            *core.launch.lock() = Some(launch);
        });
        drop(scope);
        drop(core);
    }
    let mut call =
        mac_module::abi::WorkerCall { header:
                                          mac_module::abi::Header::of::<mac_module::abi::WorkerCall>(),
                                      operation:     mac_module::abi::worker::EXIT,
                                      flags:         0,
                                      worker:        0,
                                      specification: core::ptr::null(),
                                      state:         0,
                                      exit_status:   0,
                                      microseconds:  0 };
    let status = unsafe {
        traps::gateway(mac_module::abi::worker::TRAP,
                       (&mut call as *mut mac_module::abi::WorkerCall).cast(),
                       core::mem::size_of_val(&call))
    };
    emergency::enter(crate::emergency::Kind::SystemPanic,
                     format_args!("Worker EXIT restored a completed stack unexpectedly: status \
                                   {status}."));
}

/// Commit a worker restore only while SCHEDULER owns the captured current
/// frame or an unclaimed incoming frame. Never patch remote restore storage.
pub(super) fn commit_resume(thread: &Thread)
{
    let Some(worker) = thread.worker.as_ref()
    else
    {
        return;
    };
    let mut state = worker.get();
    if state.resume_committed && !state.park_return
    {
        return;
    }
    if state.park_return
    {
        let backing = thread.core.backing.lock();
        let Some(backing) = backing.as_ref()
        else
        {
            emergency::enter(crate::emergency::Kind::SystemPanic,
                             format_args!("Worker PARK return has no CPU backing: thread {}.",
                                          thread.id));
        };
        traps::worker_resume_status(&backing.cpu,
                                    if state.cancelled
                                    {
                                        mac_abi::OPERATION_CANCELLED
                                    }
                                    else
                                    {
                                        mac_abi::OK
                                    });
        state.park_return = false;
    }
    state.resume_committed = true;
    worker.set(state);
}
/// Reap wake follows actual release of an executing hart's persistent claim.
pub(super) fn execution_released(thread: &Thread)
{
    if thread.state.get().service.failure().is_none()
       && thread.worker.as_ref().is_some_and(|worker| {
                                    let state = worker.get();
                                    state.returned
                                    && state.phase == mac_module::abi::worker::FINISHED
                                })
    {
        WORKER_PENDING.store(true, Ordering::Release);
    }
}

pub(super) fn reclaim_workers()
{
    if !WORKER_PENDING.swap(false, Ordering::AcqRel)
    {
        return;
    }
    let mut after = 0;
    loop
    {
        let candidate = {
            let scheduler = SCHEDULER.lock();
            scheduler.iter().find_map(|(slot, thread)| {
                                if slot < after
                                {
                                    return None;
                                }
                                after = slot + 1;
                                let state = thread.worker.as_ref()?.get();
                                if !state.returned
                                   || state.phase != mac_module::abi::worker::FINISHED
                                   || thread.state.get().live
                                {
                                    return None;
                                }
                                let wait = if !thread.state.get().service.quiescent()
                                {
                                    Some(ReapState::NativeService(thread.state.get().service.failure().unwrap_or(mac_abi::SERVICE_BUSY)))
                                }
                                else if let Some(hart) = thread.state.get().claimed_by
                                {
                                    Some(ReapState::Running { hart: hart.number() })
                                }
                                else
                                {
                                    let core = Arc::strong_count(&thread.core);
                                    let context = Arc::strong_count(&thread.core.context);
                                    (core != 1 || context != 1).then_some(ReapState::Readers { core,
                                                                    context })
                                };
                                Some((slot,
                                      thread.id,
                                      thread.core.clone(),
                                      thread.scope.clone(),
                                      wait))
                            })
        };
        let Some((slot, id, core, scope, wait)) = candidate
        else
        {
            break;
        };
        if let Some(wait) = wait
        {
            let previous = core.cleanup_refusal.lock().replace(wait);
            if previous != Some(wait)
            {
                serial!("MODULE_WORKER_RETIRE_WAIT thread={id} module={} reason={wait:?}",
                        core.binding.owner());
            }
            // A real claim release, not a repeated timer scan, rearms this wait.
            continue;
        }
        // A code-dependent destructor runs under its original THRD root, with
        // no scheduler/ledger/core lock held. Refusal retains the exact payload
        // and its pending walk ticket for a later retry.
        let launch = core.launch.lock().take();
        if let Some(mut launch) = launch
        {
            let result = (|| {
                let scope = scope.as_ref().ok_or(mac_abi::BAD_ABI)?;
                let subject = core.context.worker_io_subject().ok_or(mac_abi::BAD_ABI)?;
                let target = crate::allocator::Target::Owner {
                    account: launch.allocation().clone(), class: MemoryClass::Kernel
                }.in_scope(scope).map_err(|error| crate::allocation::account_status(&error))?;
                // Broker continuations keep this worker's original I/O root;
                // ordinary destructor allocations use its still-open THRD root.
                crate::context::with_io_subject(subject, || {
                    crate::context::with_allocation(target, || launch.destroy())
                }).and_then(core::convert::identity)
            })();
            if let Err(status) = result
            {
                let previous = core.cleanup_refusal.lock().replace(ReapState::Refused(status));
                if previous != Some(ReapState::Refused(status))
                {
                    serial!("MODULE_WORKER_CLEANUP_RETAINED thread={id} status={status}");
                }
                *core.launch.lock() = Some(launch);
                WORKER_PENDING.store(true, Ordering::Release);
                continue;
            }
            let backing = core.backing.lock().take();
            core.context.release();
            // Physical stack/CPU storage leaves before the retained code pin.
            drop(backing);
            drop(launch);
            WORKER_REAP_COUNT.fetch_sub(1, Ordering::AcqRel);
        }
        if core.backing.lock().is_some()
        {
            // A finished payload without its destruction capability is corrupt,
            // not proof that its unique walk ticket may be acknowledged.
            let previous =
                core.cleanup_refusal.lock().replace(ReapState::Refused(mac_abi::BAD_ABI));
            if previous != Some(ReapState::Refused(mac_abi::BAD_ABI))
            {
                serial!("MODULE_WORKER_CLEANUP_RETAINED thread={id} status={} \
                         reason=missing_launch",
                        mac_abi::BAD_ABI);
            }
            WORKER_PENDING.store(true, Ordering::Release);
            continue;
        }
        let removed = {
            let mut scheduler = SCHEDULER.lock();
            let dispose = scheduler.thread(slot)
                                   .and_then(|t| t.worker.as_ref())
                                   .is_some_and(|w| w.get().dispose);
            if !dispose
            {
                None
            }
            else
            {
                let ticket = core.release.lock().take();
                let removed = match (scheduler.records.as_mut(), ticket)
                {
                    (Some(store), Some(ticket)) =>
                    {
                        match store.release(ticket)
                        {
                            Ok(removed) => Some(removed),
                            Err(error) =>
                            {
                                let previous = core.cleanup_refusal
                                                   .lock()
                                                   .replace(ReapState::Refused(mac_abi::BAD_ABI));
                                if previous != Some(ReapState::Refused(mac_abi::BAD_ABI))
                                {
                                    serial!("MODULE_WORKER_RELEASE_RETAINED thread={id} \
                                             reason={error}");
                                }
                                *core.release.lock() = Some(error.into_ticket());
                                WORKER_PENDING.store(true, Ordering::Release);
                                None
                            }
                        }
                    }
                    (Some(store), None) => store.remove(id),
                    (None, ticket) =>
                    {
                        *core.release.lock() = ticket;
                        None
                    }
                };
                if removed.is_some()
                {
                    scheduler.threads[slot] = None;
                }
                removed
            }
        };
        let module = core.binding.owner();
        let disposed = removed.is_some();
        drop(core);
        drop(removed);
        // Backing/charges and the code-dependent capture already left above.
        // Removed destroys the final record/context before attempting ACK;
        // a refused ACK stays with the canonical completion quarantine.
        let acknowledged = disposed && scope.as_ref().is_some_and(|scope| scope.root_retired());
        // This temporary lease must leave before reporting the final disposal.
        drop(scope);
        if disposed
        {
            if acknowledged
            {
                serial!("MODULE_WORKER_RECLAIMED module={module} thread={id}");
            }
            else
            {
                serial!("MODULE_WORKER_RELEASE_RETAINED module={module} thread={id} \
                         reason=canonical_ack_not_confirmed");
            }
        }
    }
}
