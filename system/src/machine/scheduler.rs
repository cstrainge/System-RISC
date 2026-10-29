//! RV64 one-hart timer scheduler. Only this adapter owns saved CPU frames.
//! Timer interrupts preempt application code; synchronous kernel traps run with
//! interrupts masked, so no allocator/manager guard can be preempted by an app.
use alloc::{sync::Arc, vec::Vec};
use core::{arch::asm,
           cell::Cell,
           ops::{Deref, DerefMut},
           sync::atomic::{AtomicBool, AtomicUsize, Ordering}};

use mac_object_table::{domain::ReleasedResource, store::CompletionKey};
use spinning_top::Spinlock as Mutex;

use super::traps::TrapFrame;
use crate::{context::{EventWaiter, ThreadContext},
            toolbox::handles::{OwnerRef, Store}};
mod lifetime;
mod worker;
pub(crate) mod recovery;
pub(crate) use recovery::{FinderBackingLoan, initialize_finder_recovery, pump_recovery_backing,
                          take_finder_recovery};
use worker::{ReapState, ThreadName, WorkerState, cancel_worker, reclaim_workers};
pub(crate) use worker::{activate_workers, prepare_worker, publish_worker, worker_cancel,
                        worker_current, worker_dispose, worker_exit, worker_park, worker_start,
                        worker_status, worker_wake};
pub(crate) mod accounting;
pub(crate) use lifetime::{ReleaseThreads, specification};

use super::traps::execution::{HartKey, IDLE, current, idle_frame, in_io, install_idle,
                              select_current, selected_hart};
pub(crate) use super::traps::execution::{activate_context, authenticate_module, critical_active,
                                         current_context, io_enter, io_leave,
                                         module_callback_allowed, module_owner,
                                         reserve_allocation, reserve_replacement};
// Generated from the same checked platform policy as the linker artifacts.
const STACK_BYTES: usize = super::memory::APPLICATION_STACK_BYTES as usize;
// The current platform time counter runs at 10 MHz. At 500 us, two runnable peers can consume the
// entire 1 ms interaction budget before a queued caller resumes. Use 250 us
// for this resident round-robin policy; a freshly woken input consumer still
// takes priority. This is not a hard real-time or future scheduler contract.
const TIMER_INTERVAL: u64 = 2_500;
static RECLAIM_PENDING: AtomicBool = AtomicBool::new(false);
static RETIRED_COUNT: AtomicUsize = AtomicUsize::new(0);
static WORKER_REAP_COUNT: AtomicUsize = AtomicUsize::new(0);
static INPUT_PRIORITY: crate::toolbox::events::priority::InputPriority =
    crate::toolbox::events::priority::InputPriority::new();
struct Thread
{
    id:     u64,
    core:   Arc<ThreadCore>,
    state:  Cell<RunState>,
    worker: Option<Cell<WorkerState>>,
    scope:  Option<mac_object_table::scope::Lease<crate::toolbox::handles::InvocationGate>>
}
#[derive(Clone, Copy)]
struct RunState
{
    // Scheduler-owned execution claim, retained across frame restore and body
    // execution. Only a complete outer-capture handoff may clear it.
    claimed_by: Option<HartKey>,
    live:       bool,
    retired:    bool,
    switches:   u64,
    waiting:    Option<Wait>,
    woken:      u8
}
impl Thread
{
    fn update(&self, change: impl FnOnce(&mut RunState))
    {
        let mut state = self.state.get();
        change(&mut state);
        self.state.set(state);
    }
}
#[derive(Clone, Copy)]
enum Wait
{
    Worker,
    Events
    {
        mask:     u16,
        deadline: u64
    },
    Io
    {
        job: crate::context::DeferredIo
    }
}
struct Scheduler
{
    // Positions select CPU backing only; genuine facility receipts authorize
    // access even after ordinary IDs have been invalidated by the owner walk.
    threads:        Vec<Option<CompletionKey>>,
    records:        Option<Store<Thread>>,
    waiters:        Vec<EventWaiter>,
    waiters_leased: bool,
    ticks:          u64
}
impl Scheduler
{
    fn thread(&self, slot: usize) -> Option<&Thread>
    {
        self.records.as_ref()?.completing(self.threads.get(slot)?.as_ref()?)
    }

    fn iter(&self) -> impl Iterator<Item = (usize, &Thread)>
    {
        self.threads.iter().enumerate().filter_map(|(slot, _)| self.thread(slot).map(|t| (slot, t)))
    }

    fn prepare(&mut self) -> Result<usize, mac_abi::Status>
    {
        if self.records.is_none()
        {
            let (objects, backing) = crate::toolbox::handles::thread_kinds()?;
            self.records = Some(Store::owned(objects, backing).map_err(lifetime::pool_status)?);
        }
        let slot = self.threads.iter().position(Option::is_none).unwrap_or(self.threads.len());
        if slot == self.threads.len()
        {
            self.threads.try_reserve(1).map_err(|_| mac_abi::NO_MEMORY)?;
        }
        // Admission grows scratch, never the timer/event polling path. If input
        // handling births a thread while a snapshot is leased, reserve a spare;
        // returning the lease keeps the larger buffer.
        self.waiters
            .try_reserve(slot.saturating_add(1).saturating_sub(self.waiters.len()))
            .map_err(|_| mac_abi::NO_MEMORY)?;
        Ok(slot)
    }
}
static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler { records:        None,
                                                            threads:        Vec::new(),
                                                            waiters:        Vec::new(),
                                                            waiters_leased: false,
                                                            ticks:          0 });
unsafe extern "C" {
    fn __system_start_frame(frame: *mut TrapFrame) -> !;
}
pub(crate) fn context_released()
{
    if RETIRED_COUNT.load(Ordering::Acquire) != 0
    {
        RECLAIM_PENDING.store(true, Ordering::Release);
    }
    worker::context_released();
}
pub fn switches(app: u64) -> u64
{
    SCHEDULER.lock()
             .iter()
             .find_map(|(_, t)| {
                 (t.state.get().live && t.core.context.identity().app.0 == app).then(|| {
                                                                                   t.state
                                                                                    .get()
                                                                                    .switches
                                                                               })
             })
             .unwrap_or(0)
}
/// Retained stacks and invocation readers remain part of the process barrier.
pub(crate) fn has_threads(app: u64) -> bool
{
    SCHEDULER.lock().iter().any(|(_, t)| t.core.context.identity().app.0 == app)
}
/// Own every fallible stack/image preparation before publishing a thread.
/// The plan can wait for a retiring slot without consuming its image use.
struct ThreadBacking
{
    // CPU/stack disappear before code-dependent launch/image pins.
    cpu:    accounting::Charged<super::traps::SavedState>,
    stack:  accounting::Charged<super::pages::Stack>,
    loaded: Option<crate::kernel::LoadedImage>
}
struct ThreadCore
{
    backing:         Mutex<Option<ThreadBacking>>,
    launch:          Mutex<Option<crate::drivers::WorkerLaunch>>,
    context:         Arc<ThreadContext>,
    release:         Mutex<Option<ReleasedResource>>,
    binding:         crate::images::Binding,
    top:             usize,
    cleanup_refusal: Mutex<Option<ReapState>>,
    name:            Option<ThreadName>
}
impl ThreadCore
{
    fn cpu_frame(&self) -> Option<*mut TrapFrame>
    {
        let backing = self.backing.lock();
        let backing = backing.as_ref()?;
        Some(backing.cpu.frame())
    }

    // Only a claimed row is published into this hart's restore/entry control.
    // Preflight obtained its frame under SCHEDULER; disappearance here is
    // structural corruption, never permission to restore unowned storage.
    fn publish_cpu(&self)
    {
        let backing = self.backing.lock();
        let Some(backing) = backing.as_ref()
        else
        {
            super::emergency::enter(crate::emergency::Kind::SystemPanic,
                                    format_args!("Claimed thread lost CPU backing before frame \
                                                  publication."));
        };
        super::traps::publish_cpu(&backing.cpu);
    }
}
pub(crate) struct PreparedThread
{
    owner:      crate::toolbox::handles::OwnerRef,
    core:       mac_object_table::retained::Read<ThreadCore>,
    allocation: accounting::AllocationAuthority,
    recovery:   Option<crate::allocator::recovery::Permit>
}
pub(crate) struct PrepareFailure
{
    pub(crate) status: mac_abi::Status,
    pub(crate) loaded: crate::kernel::LoadedImage
}
pub(crate) struct SpawnFailure
{
    pub(crate) status:   mac_abi::Status,
    pub(crate) prepared: PreparedThread
}
pub(crate) fn prepare(owner: crate::toolbox::handles::OwnerRef,
                      loaded: crate::kernel::LoadedImage, charges: accounting::ThreadCharges)
                      -> Result<PreparedThread, PrepareFailure>
{
    let target = crate::allocator::Target::Owner { account: charges.allocation().clone(),
                                                   class:
                                                       mac_kernel_model::policy::MemoryClass::Kernel };
    // The child's genuine account scopes BOTH Arc allocations. Keep loaded
    // borrowed until every fallible preparation has succeeded, so refusal can
    // return the original image use without publishing or losing it.
    let prepared = crate::context::with_allocation(target, || {
        if !owner.accepting() || loaded.binding().owner() != owner.owner().incarnation()
        {
            return Err(mac_abi::ACCESS_DENIED);
        }
        let context = Arc::try_new(ThreadContext::new()).map_err(|_| mac_abi::NO_MEMORY)?;
        let (stack, cpu, allocation) = charges.allocate(&owner, |top| {
                                                  let mut initial = TrapFrame::default();
                                                  initial.pc = app_start as *const () as usize;
                                                  initial.status = super::traps::INITIAL_STATUS;
                                                  initial.registers[8] = loaded.entry();
                                                  initial.stack_pointer = top;
                                                  // gp is unused by the freestanding PC-relative
                                                  // executable contract.
                                                  initial
                                              })
                                              .map_err(|error| {
                                                  serial!("THREAD_BACKING_REFUSED: {error}");
                                                  error.status()
                                              })?;
        let top = stack.top();
        let core = Arc::try_new(ThreadCore { backing:
                                                 Mutex::new(Some(ThreadBacking { cpu,
                                                                                 stack,
                                                                                 loaded: None })),
                                             launch: Mutex::new(None),
                                             context,
                                             release: Mutex::new(None),
                                             binding: loaded.binding(),
                                             top,
                                             cleanup_refusal: Mutex::new(None),
                                             name: None }).map_err(|_| mac_abi::NO_MEMORY)?;
        Ok((core, allocation))
    });
    match prepared
    {
        Ok((mut core, allocation)) =>
        {
            let Some(storage) = Arc::get_mut(&mut core)
            else
            {
                serial!("THREAD_CORE_REFUSED: freshly allocated preparation is not exclusive");
                return Err(PrepareFailure { status: mac_abi::BAD_ABI,
                                            loaded });
            };
            match storage.backing.get_mut().as_mut()
            {
                Some(backing) => backing.loaded = Some(loaded),
                None =>
                {
                    return Err(PrepareFailure { status: mac_abi::BAD_ABI,
                                                loaded });
                }
            }
            Ok(PreparedThread { owner,
                                core: mac_object_table::retained::Read::new(core,
                                                                            context_released),
                                allocation,
                                recovery: None })
        }
        Err(status) =>
        {
            Err(PrepareFailure { status,
                                 loaded })
        }
    }
}
pub(crate) struct RecoveryPrepareFailure
{
    pub(crate) status: mac_abi::Status,
    pub(crate) loaded: crate::kernel::LoadedImage,
    pub(crate) loan:   FinderBackingLoan
}
/// The same process entry, CPU frame and THRD publication use boot-held native
/// backing. All fallible control-block work precedes consumption of the loan.
pub(crate) fn prepare_recovery(owner: OwnerRef, loaded: crate::kernel::LoadedImage,
                               charges: accounting::ThreadCharges, loan: FinderBackingLoan)
                               -> Result<PreparedThread, RecoveryPrepareFailure>
{
    let permit = loan.permit();
    let target = crate::allocator::Target::Owner { account: charges.allocation().clone(),
                                                   class:
                                                       mac_kernel_model::policy::MemoryClass::Kernel };
    let metadata = crate::context::with_recovery_allocation(permit.clone(), || {
        crate::context::with_allocation(target, || {
            if !owner.accepting()
               || owner.owner().kind() != mac_object_table::domain::DomainKind::Process
               || owner.owner().incarnation() != crate::context::finder_app().0
               || loaded.binding().owner() != owner.owner().incarnation()
            {
                return Err(mac_abi::ACCESS_DENIED);
            }
            let context = Arc::try_new(ThreadContext::new()).map_err(|_| mac_abi::NO_MEMORY)?;
            context.install_recovery_default(permit.clone());
            let core = ThreadCore { backing: Mutex::new(None),
                                    launch: Mutex::new(None),
                                    context,
                                    release: Mutex::new(None),
                                    binding: loaded.binding(),
                                    top: 0,
                                    cleanup_refusal: Mutex::new(None),
                                    name: None };
            Arc::try_new(core).map_err(|_| mac_abi::NO_MEMORY)
        })
    });
    let mut core = match metadata
    {
        Ok(core) => core,
        Err(status) =>
        {
            return Err(RecoveryPrepareFailure { status,
                                                loaded,
                                                loan });
        }
    };
    let Some(storage) = Arc::get_mut(&mut core)
    else
    {
        return Err(RecoveryPrepareFailure { status: mac_abi::BAD_ABI,
                                            loaded,
                                            loan });
    };
    let backing = charges.allocate_recovery(&owner, loan, |top| {
                             let mut frame = TrapFrame::default();
                             frame.pc = app_start as *const () as usize;
                             frame.status = super::traps::INITIAL_STATUS;
                             frame.registers[8] = loaded.entry();
                             frame.stack_pointer = top;
                             frame
                         });
    let (stack, cpu, allocation) = match backing
    {
        Ok(backing) => backing,
        Err((error, loan)) =>
        {
            return Err(RecoveryPrepareFailure { status: error.status(),
                                                loaded,
                                                loan });
        }
    };
    storage.top = stack.top();
    *storage.backing.get_mut() = Some(ThreadBacking { cpu,
                                                      stack,
                                                      loaded: Some(loaded) });
    Ok(PreparedThread { owner,
                        core: mac_object_table::retained::Read::new(core, context_released),
                        allocation,
                        recovery: Some(permit) })
}

pub(crate) fn spawn_prepared(prepared: PreparedThread, runnable: bool)
                             -> Result<u64, SpawnFailure>
{
    match prepared.recovery.clone()
    {
        Some(permit) =>
        {
            crate::context::with_recovery_allocation(permit, || {
                spawn_prepared_inner(prepared, runnable)
            })
        }
        None => spawn_prepared_inner(prepared, runnable)
    }
}
fn spawn_prepared_inner(prepared: PreparedThread, runnable: bool) -> Result<u64, SpawnFailure>
{
    let published =
        (|| {
            let mut scheduler = SCHEDULER.lock();
            let slot = crate::allocator::with_system(crate::allocator::SystemClass::Ordinary,
                                                     || scheduler.prepare())?;
            if !prepared.owner.accepting()
            {
                return Err(mac_abi::ACCESS_DENIED);
            }
            let store = scheduler.records.as_mut().ok_or(mac_abi::BAD_ABI)?;
            // Shared Store indices grow under their narrow System hook. Its actual
            // per-owner Claim, slots and completions belong to this admitted child.
            let publication = crate::context::with_allocation(crate::allocator::Target::Owner {
                account: prepared.allocation.clone(),
                class: mac_kernel_model::policy::MemoryClass::Kernel
            }, || store.insert_with_receipt(&prepared.owner, |id| {
                Thread { id,
                         core: prepared.core.share(),
                         worker: None, scope: None,
                         state: Cell::new(RunState { claimed_by: None, live: false,
                                                    retired: false,
                                                    switches: 0,
                                                    waiting: None,
                                                    woken: 0 }) }
            })).map_err(lifetime::pool_status)?;
            let (id, receipt) = publication.into_parts();
            if slot == scheduler.threads.len()
            {
                scheduler.threads.push(Some(receipt));
            }
            else
            {
                scheduler.threads[slot] = Some(receipt);
            }
            // Publication's receipt remains valid if a stopping walk has already
            // invalidated ordinary lookup. An unstarted refusal retains the same
            // preparation; its stack/image pins are never dropped under this lock.
            let assigned = prepared.core.context.assign(prepared.owner.clone(),
                                                        id,
                                                        prepared.core.binding,
                                                        &prepared.allocation);
            if let Err(status) = assigned
            {
                let removed = scheduler.records.as_mut().and_then(|store| store.remove(id));
                if removed.is_some()
                {
                    scheduler.threads[slot] = None;
                }
                else if let Some(thread) = scheduler.thread(slot)
                {
                    retire(thread);
                }
                drop(scheduler);
                drop(removed);
                return Err(status);
            }
            if let Some(thread) = scheduler.thread(slot)
            {
                thread.update(|state| state.live = runnable);
            }
            Ok(id)
        })();
    match published
    {
        Ok(id) =>
        {
            let backing = prepared.core.backing.lock();
            if let Some(backing) = backing.as_ref()
            {
                serial!("PROCESS_THREAD app={} thread={} stack=0x{:x} stack_bytes={} \
                         stack_backing_bytes={} cpu_state=0x{:x} cpu_state_bytes={} \
                         cpu_base=0x{:x} cpu_backing_bytes={} stack_physical={:#x} \
                         cpu_physical={:#x}",
                        prepared.owner.owner().incarnation(),
                        id,
                        prepared.core.top,
                        backing.stack.bytes(),
                        backing.stack.backing_bytes(),
                        backing.cpu.frame() as usize,
                        backing.cpu.bytes(),
                        backing.cpu.base(),
                        backing.cpu.backing_bytes(),
                        backing.stack.physical().0,
                        backing.cpu.physical());
            }
            Ok(id)
        }
        Err(status) =>
        {
            Err(SpawnFailure { status,
                               prepared })
        }
    }
}
pub(crate) fn start_prepared(thread: u64, owner: &OwnerRef) -> Result<(), mac_abi::Status>
{
    let scheduler = SCHEDULER.lock();
    let record =
        scheduler.records.as_ref().and_then(|store| store.get(thread)).ok_or(mac_abi::NOT_FOUND)?;
    if !owner.accepting() || record.core.context.identity().app.0 != owner.owner().incarnation()
    {
        return Err(mac_abi::ACCESS_DENIED);
    }
    if record.state.get().retired
    {
        return Err(mac_abi::SERVICE_BUSY);
    }
    record.update(|state| state.live = true);
    Ok(())
}
pub(crate) fn spawn(owner: OwnerRef, loaded: crate::kernel::LoadedImage,
                    charges: accounting::ThreadCharges)
                    -> Result<u64, mac_abi::Status>
{
    let prepared = prepare(owner, loaded, charges).map_err(|failure| {
                                                      let PrepareFailure { status,
                                                                           loaded } = failure;
                                                      // Failed preparation retains the unique image
                                                      // until its caller accepts
                                                      // the refusal; dispose that lease through its
                                                      // ordinary owned path.
                                                      drop(loaded);
                                                      status
                                                  })?;
    spawn_prepared(prepared, true).map_err(|failure| failure.status)
}

extern "C" fn app_start(entry: usize) -> !
{
    let function: mac_abi::ApplicationEntry = unsafe { core::mem::transmute(entry) };
    let _status = unsafe { function(crate::kernel::services()) };
    let mut call = mac_abi::processes::ProcessCall { operation: mac_abi::processes::EXIT,
                                                     ..Default::default() };
    unsafe {
        super::traps::gateway(mac_abi::traps::PROCESS,
                              (&mut call as *mut mac_abi::processes::ProcessCall).cast(),
                              core::mem::size_of_val(&call));
    }
    loop
    {
        core::hint::spin_loop();
    }
}
fn arm()
{
    let deadline = crate::platform::ticks().wrapping_add(TIMER_INTERVAL);
    // Sstc was admitted by firmware before supervisor entry.
    unsafe {
        asm!("csrw 0x14d,{}",in(reg)deadline,options(nostack));
    }
}

pub fn start() -> mac_formats::Result<()>
{
    let scheduler = SCHEDULER.lock();
    let stack = super::pages::Stack::allocate(STACK_BYTES).map_err(|error| {
                    serial!("IDLE_STACK_REFUSED: {error}");
                    mac_formats::Error::AllocationFailed("physical page allocation refused for \
                                                          idle stack")
                })?;
    let top = stack.top();
    let mut initial = TrapFrame::default();
    initial.pc = idle as *const () as usize;
    initial.status = super::traps::INITIAL_STATUS;
    initial.stack_pointer = top;
    let cpu = super::traps::SavedState::new(initial).map_err(|error| {
                  serial!("IDLE_CPU_STATE_REFUSED: {error}");
                  mac_formats::Error::AllocationFailed("physical page allocation refused for idle \
                                                        CPU state")
              })?;
    install_idle(stack, cpu).map_err(|status| {
                                serial!("IDLE_INSTALL_REFUSED status={status}");
                                mac_formats::Error::Malformed("scheduler idle backing does not \
                                                               match its admitted hart")
                            })?;
    // Module preparations can precede Finder in the shared THRD store. Start
    // the genuine runnable Finder process, never an inert/armed module slot.
    let finder = crate::context::finder_app().0;
    let (slot, t) =
        scheduler.iter()
                 .find(|(_, thread)| {
                     let state = thread.state.get();
                     thread.worker.is_none()
                     && thread.core.context.identity().app.0 == finder
                     && state.live
                     && !state.retired
                 })
                 .ok_or(mac_formats::Error::Malformed("scheduler bootstrap has no runnable \
                                                       Finder process"))?;
    let frame =
        t.core
         .cpu_frame()
         .ok_or(mac_formats::Error::Malformed("scheduler bootstrap Finder has no CPU backing"))?;
    let hart = selected_hart();
    if t.state.get().claimed_by.is_some()
    {
        return Err(mac_formats::Error::Malformed("scheduler bootstrap Finder is already claimed"));
    }
    t.update(|state| {
         state.claimed_by = Some(hart);
         state.switches += 1;
     });
    t.core.publish_cpu();
    activate_context(Some(&t.core.context));
    select_current(slot);
    drop(scheduler);
    arm();
    unsafe {
        asm!("csrs sie, {}",in(reg)32usize,options(nomem,nostack));
        __system_start_frame(frame)
    }
}
extern "C" fn idle() -> !
{
    loop
    {
        // No application is runnable. Only this dedicated stack executes WFI;
        // an application can never suspend while holding a resident guard.
        unsafe {
            asm!("wfi", options(nomem, nostack));
        }
    }
}

/// Called only by the outermost native wait trap, with interrupts masked.
pub fn wait_current(mask: u16, timeout_ticks: u32)
{
    if timeout_ticks == 0
    {
        return;
    }
    let deadline = crate::platform::ticks()
        .saturating_add((u64::from(timeout_ticks) * 10_000_000).div_ceil(60));
    let index = current();
    if let Some(thread) = SCHEDULER.lock().thread(index)
    {
        thread.update(|state| {
                  state.waiting = Some(Wait::Events { mask,
                                                      deadline });
                  state.woken = 0;
              });
    }
}
pub fn blocked() -> bool
{
    SCHEDULER.lock().thread(current()).is_some_and(|thread| thread.state.get().waiting.is_some())
}
/// Own a preallocated snapshot while Desktop routes input without the scheduler
/// lock. Drop returns scratch even when the Desktop guard refuses this pass.
pub(crate) struct Waiting
{
    rows: Vec<EventWaiter>
}
impl Deref for Waiting
{
    type Target = [EventWaiter];

    fn deref(&self) -> &[EventWaiter]
    {
        &self.rows
    }
}
impl DerefMut for Waiting
{
    fn deref_mut(&mut self) -> &mut [EventWaiter]
    {
        &mut self.rows
    }
}
impl Drop for Waiting
{
    fn drop(&mut self)
    {
        self.rows.clear();
        let mut scheduler = SCHEDULER.lock();
        if self.rows.capacity() > scheduler.waiters.capacity()
        {
            core::mem::swap(&mut self.rows, &mut scheduler.waiters);
        }
        scheduler.waiters_leased = false;
        // The unused smaller allocation drops after the scheduler guard.
    }
}
pub fn waiting() -> Option<Waiting>
{
    let mut scheduler = SCHEDULER.lock();
    if scheduler.waiters_leased
    {
        return None;
    }
    let mut rows = core::mem::take(&mut scheduler.waiters);
    for (_, thread) in scheduler.iter()
    {
        let state = thread.state.get();
        if state.live
           && let Some(Wait::Events { mask,
                                      .. }) = state.waiting
        {
            rows.push(EventWaiter { app: thread.core.context.identity().app.0,
                                    mask,
                                    ready: false });
        }
    }
    scheduler.waiters_leased = true;
    Some(Waiting { rows })
}

pub fn wake(app: u64)
{
    wake_with_priority(app, 1);
}

/// Foreground input outranks background activation/update notifications.
pub fn wake_interactive(app: u64)
{
    wake_with_priority(app, 2);
}

fn wake_with_priority(app: u64, priority: u8)
{
    for (_, thread) in SCHEDULER.lock().iter()
    {
        let state = thread.state.get();
        if thread.core.binding.owner() == app
           && state.live
           && matches!(state.waiting, Some(Wait::Events { .. }))
        {
            thread.update(|state| {
                      state.waiting = None;
                      state.woken = priority;
                  });
            break;
        }
    }
}
/// Park only after the original trap has dropped every manager guard. The
/// replay keeps the original PC/registers, and borrows fresh buffers on wake.
/// Checking completion while holding the scheduler guard closes completion-
/// before-park races; the job pump releases its queue guard before waking us.
pub fn wait_io(job: crate::context::DeferredIo) -> Result<bool, mac_abi::Status>
{
    let index = current();
    let scheduler = SCHEDULER.lock();
    let thread = scheduler.thread(index)
                          .filter(|thread| thread.state.get().live)
                          .ok_or(mac_abi::WRONG_THREAD)?;
    let pending = match job
    {
        crate::context::DeferredIo::FileTransfer(handle) =>
        {
            crate::toolbox::files::transfer_io_pending(thread.id, handle)?
        }
        crate::context::DeferredIo::DriverJob(handle) =>
        {
            crate::drivers::job_pending(thread.id, handle)?
        }
    };
    if !pending
    {
        return Ok(false);
    }
    thread.update(|state| {
              state.waiting = Some(Wait::Io { job });
              state.woken = 0;
          });
    Ok(true)
}
pub fn wake_io(identity: u64, job: crate::context::DeferredIo)
{
    let scheduler = SCHEDULER.lock();
    if let Some(thread) = scheduler.records.as_ref().and_then(|store| store.get(identity))
       && thread.state.get().live
       && matches!(thread.state.get().waiting, Some(Wait::Io { job: expected }) if expected == job)
    {
        thread.update(|state| {
                  state.waiting = None;
                  state.woken = 1;
              });
    }
}
fn retire(thread: &Thread)
{
    if !thread.state.get().retired
    {
        thread.update(|state| {
                  state.live = false;
                  state.retired = true;
              });
        RETIRED_COUNT.fetch_add(1, Ordering::AcqRel);
    }
    RECLAIM_PENDING.store(true, Ordering::Release);
}
/// Stop execution before dispatching the common ledger walk. Each real thread
/// retains its original job cancellation owner, including after invalidation.
pub(crate) fn retire_owner(owner: u64)
{
    let mut after = 0;
    loop
    {
        let retired = {
            let scheduler = SCHEDULER.lock();
            scheduler.iter()
                     .find(|(slot, t)| {
                         *slot >= after
                         && !t.state.get().retired
                         && t.core.context.identity().app.0 == owner
                     })
                     .map(|(slot, t)| {
                         retire(t);
                         after = slot + 1;
                         t.id
                     })
        };
        let Some(id) = retired
        else
        {
            break;
        };
        crate::drivers::discard_thread_jobs(id);
    }
}
/// Reuse requires the original walk ticket, a noncurrent stack and no context /
/// preparation readers. Removed's destructor drops the stack and code pins
/// outside the scheduler lock before acknowledging that unique ticket.
pub(crate) fn reclaim_retired()
{
    if critical_active()
    {
        return;
    }
    if !RECLAIM_PENDING.swap(false, Ordering::AcqRel)
    {
        return;
    }
    let mut after = 0;
    loop
    {
        let removed = {
            let mut scheduler = SCHEDULER.lock();
            let found = scheduler.iter().find_map(|(slot, thread)| {
                                            if slot < after || !thread.state.get().retired
                                            {
                                                return None;
                                            }
                                            after = slot + 1;
                                            if thread.state.get().claimed_by.is_some()
                                            {
                                                // The actual handoff rearms reap after releasing
                                                // this claim, independent of this reaper's hart.
                                                return None;
                                            }
                                            if Arc::strong_count(&thread.core) != 1
                                               || Arc::strong_count(&thread.core.context) != 1
                                            {
                                                return None;
                                            }
                                            thread.core.release.lock().take().map(|ticket| {
                                                                                 (slot,
                                                                                  ticket,
                                                                                  thread.core
                                                                                        .clone())
                                                                             })
                                        });
            let Some((slot, ticket, core)) = found
            else
            {
                break;
            };
            // This exact store/receipt was checked above under the same lock.
            let result = match scheduler.records.as_mut()
            {
                Some(store) =>
                {
                    match store.release(ticket)
                    {
                        Ok(removed) => Ok(removed),
                        Err(error) =>
                        {
                            serial!("THREAD_RECLAIM_REFUSED: {error}; retaining backing in the \
                                     common owner quarantine");
                            match store.quarantine(error.into_ticket())
                            {
                                Ok(()) => Err(None),
                                Err(error) =>
                                {
                                    serial!("THREAD_QUARANTINE_REFUSED: {error}; original ticket \
                                             remains with its actual thread backing");
                                    Err(Some(error.into_ticket()))
                                }
                            }
                        }
                    }
                }
                None => Err(Some(ticket))
            };
            match result
            {
                Ok(removed) =>
                {
                    scheduler.threads[slot] = None;
                    RETIRED_COUNT.fetch_sub(1, Ordering::AcqRel);
                    // The temporary read must leave before Removed destroys
                    // its payload and acknowledges stack/code quiescence.
                    drop(core);
                    Some(removed)
                }
                Err(ticket) =>
                {
                    // Restore through the retained real backing, never a
                    // second lookup which could discard the cleanup ticket.
                    *core.release.lock() = ticket;
                    None
                }
            }
        };
        if let Some(thread) = removed
        {
            let app = thread.core.context.identity().app.0;
            let id = thread.id;
            thread.core.context.release();
            drop(thread);
            serial!("PROCESS_THREAD_RECLAIMED app={app} thread={id}");
            crate::toolbox::processes::retirement::thread_reclaimed();
        }
    }
}

fn claim_lost(hart: HartKey, slot: usize, reason: &str) -> !
{
    super::emergency::enter(crate::emergency::Kind::SystemPanic,
                            format_args!("Scheduler execution claim invariant: hart {}, slot \
                                          {}, {}.",
                                         hart.number(),
                                         slot,
                                         reason));
}
fn validate_capture(scheduler: &Scheduler, capture: &super::traps::OuterCapture)
{
    let hart = capture.hart();
    let slot = capture.current();
    if selected_hart() != hart || current() != slot
    {
        claim_lost(hart, slot, "selected execution changed during outer capture");
    }
    if slot == IDLE
    {
        return;
    }
    let Some(thread) = scheduler.thread(slot)
    else
    {
        claim_lost(hart, slot, "captured THRD row is absent");
    };
    if thread.state.get().claimed_by != Some(hart)
    {
        claim_lost(hart, slot, "captured THRD belongs to another execution");
    }
    if thread.core
             .backing
             .lock()
             .as_ref()
             .is_none_or(|backing| backing.cpu.frame() != capture.frame())
    {
        claim_lost(hart, slot, "captured frame does not belong to the claimed THRD");
    }
}
fn release_claim(scheduler: &Scheduler, slot: usize, hart: HartKey)
{
    if slot == IDLE
    {
        return;
    }
    let Some(thread) = scheduler.thread(slot)
    else
    {
        claim_lost(hart, slot, "handoff lost its retained THRD row");
    };
    if thread.state.get().claimed_by != Some(hart)
    {
        claim_lost(hart, slot, "handoff does not own the old THRD");
    }
    thread.update(|state| state.claimed_by = None);
    if thread.state.get().retired
    {
        RECLAIM_PENDING.store(true, Ordering::Release);
    }
    worker::execution_released(thread);
}

pub(super) fn schedule(capture: super::traps::OuterCapture) -> *mut TrapFrame
{
    let frame = capture.frame();
    let hart = capture.hart();
    let current = capture.current();
    validate_capture(&SCHEDULER.lock(), &capture);
    // The admitted hart selection still names the interrupted app until a new CPU frame is
    // selected. Generic scheduler maintenance uses permanent System authority; provider
    // callbacks must explicitly restore their own retained subject selection.
    crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
        // Device input and cursor publication precede all ordinary maintenance.
        // Never enable nested preemption while a resident manager owns its guards.
        let fresh_input = crate::kernel::poll_waiting_events();
        if INPUT_PRIORITY.background_allowed(fresh_input)
        {
            reclaim_workers();
            reclaim_retired();
            crate::drivers::pump_jobs();
            crate::toolbox::files::pump_provider_operations();
            crate::toolbox::runtime::Desktop::pump_standard_file();
            pump_recovery_backing();
            crate::recovery::pump();
            crate::toolbox::crashes::pump_dismissals();
            crate::toolbox::crashes::pump_reports();
            crate::toolbox::processes::documents::pump();
            crate::toolbox::processes::retirement::pump();
            crate::toolbox::processes::finder::pump();
        }
    });
    let mut scheduler = SCHEDULER.lock();
    validate_capture(&scheduler, &capture);
    scheduler.ticks += 1;
    let now = crate::platform::ticks();
    for (_, thread) in scheduler.iter()
    {
        if matches!(thread.state.get().waiting, Some(Wait::Events { deadline, .. }) if now >= deadline)
        {
            thread.update(|state| state.waiting = None);
        }
    }
    let priority = scheduler.iter()
                            .filter(|(slot, t)| {
                                let state = t.state.get();
                                state.live
                                && (state.claimed_by.is_none()
                                    || (*slot == current && state.claimed_by == Some(hart)))
                            })
                            .map(|(_, t)| t.state.get().woken)
                            .max()
                            .unwrap_or(0);
    let count = scheduler.threads.len();
    // IDLE is a sentinel, never an issued identity or a modulo arithmetic input.
    let first = if current == IDLE { 0 } else { (current + 1) % count.max(1) };
    for boosted in [true, false]
    {
        if boosted && priority == 0
        {
            continue;
        }
        for offset in 0..count
        {
            let index = (first + offset) % count;
            let Some(thread) = scheduler.thread(index)
            else
            {
                continue;
            };
            let state = thread.state.get();
            if state.live
               && !state.retired
               && (state.claimed_by.is_none()
                   || (index == current && state.claimed_by == Some(hart)))
               && if boosted { state.woken == priority } else { state.waiting.is_none() }
            {
                let next = if index == current
                {
                    frame
                }
                else
                {
                    let Some(next) = thread.core.cpu_frame()
                    else
                    {
                        continue;
                    };
                    next
                };
                // No fallible step follows this commit. Park-return writes
                // occur only with a captured/unclaimed frame, never during sret.
                worker::commit_resume(thread);
                thread.update(|state| {
                          state.claimed_by = Some(hart);
                          state.woken = 0;
                          state.switches += 1;
                      });
                thread.core.publish_cpu();
                activate_context(Some(&thread.core.context));
                select_current(index);
                if index != current
                {
                    release_claim(&scheduler, current, hart);
                }
                arm();
                return next;
            }
        }
    }
    let next = if current == IDLE { frame } else { idle_frame() };
    activate_context(None);
    select_current(IDLE);
    release_claim(&scheduler, current, hart);
    arm();
    next
}

pub fn retired() -> bool
{
    SCHEDULER.lock().thread(current()).is_some_and(|t| t.state.get().retired)
}
pub fn image_for(app: u64) -> Option<crate::images::Binding>
{
    SCHEDULER.lock().iter().find_map(|(_, thread)| {
                               (thread.core.binding.owner() == app).then(|| {
                                                                       thread.core.context.image()
                                                                   })
                                                                   .flatten()
                           })
}
