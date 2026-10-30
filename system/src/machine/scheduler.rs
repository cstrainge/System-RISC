//! RV64 one-hart timer scheduler. Only this adapter owns saved CPU frames.
//! Timer interrupts preempt application code; synchronous kernel traps run with
//! interrupts masked, so no allocator/manager guard can be preempted by an app.
use alloc::{sync::Arc, vec::Vec};
use core::{arch::asm,
           cell::Cell,
           ops::{Deref, DerefMut},
           sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}};

use mac_object_table::{domain::ReleasedResource, store::CompletionKey};
use spinning_top::Spinlock as Mutex;

use super::traps::TrapFrame;
use crate::{context::{EventWaiter, ThreadContext},
            toolbox::handles::{OwnerRef, Store}};
mod lifetime;
pub(crate) mod application;
pub(crate) mod address;
pub(crate) use address::changed as address_wait_changed;
mod stack;
pub(crate) use stack::{StackPinError, StackWaitPin, pin_application_stack};
mod worker;
pub(crate) mod service;
pub(crate) mod wake;
pub(crate) mod policy;
include!(concat!(env!("OUT_DIR"), "/platform-scheduling.rs"));
pub(crate) mod recovery;
pub(crate) use recovery::{FinderBackingLoan, initialize_finder_recovery, pump_recovery_backing,
                          take_finder_recovery};
use worker::{ReapState, ThreadName, WorkerState, cancel_worker, reclaim_workers};
pub(crate) use worker::{activate_workers, prepare_worker, publish_worker, worker_cancel,
                        worker_current, worker_dispose, worker_exit, worker_park, worker_start,
                        worker_status, worker_wake};
pub(crate) mod accounting;
pub(crate) use lifetime::{ReleaseThreads, specification};

use super::traps::execution::{HartKey, IDLE, activate_idle_context, current, idle_frame, in_io,
                              select_current, selected_hart};
pub(crate) use super::traps::execution::{activate_context, authenticate_module, critical_active,
                                         current_context, io_enter, io_leave,
                                         module_callback_allowed, module_owner,
                                         reserve_allocation, reserve_replacement};
// Generated from the same checked platform policy as the linker artifacts.
const STACK_BYTES: usize = super::memory::APPLICATION_STACK_BYTES as usize;
const IDLE_STACK_BYTES: usize = super::memory::SYSTEM_STACK_BYTES as usize;
// Convert the checked platform quantum through the admitted DT clock once,
// before any frame is dispatched. QEMU defaults to 250 us; a freshly woken
// input consumer still takes priority in bootstrap round robin. This is not
// a hard real-time latency or future scheduler contract.
static TIMER_INTERVAL: AtomicU64 = AtomicU64::new(0);
fn timer_interval() -> u64
{
    let ticks = TIMER_INTERVAL.load(Ordering::Acquire);
    if ticks == 0
    {
        super::traps::entry_lost("scheduler frame commitment preceded validated platform timer \
                                  initialization");
    }
    ticks
}
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
/// The one THRD store contains scheduled executors and reset-lifetime affine
/// System idle owners. Run-queue positions can resolve only Scheduled records.
enum Record
{
    Scheduled(Thread),
    SystemIdle(IdleRecord)
}
impl Record
{
    fn scheduled(&self) -> Option<&Thread>
    {
        match self
        {
            Self::Scheduled(thread) => Some(thread),
            Self::SystemIdle(_) => None
        }
    }
}
struct IdleRecord
{
    id:   u64,
    core: Arc<SystemIdleCore>
}
struct SystemIdleCore
{
    cpu:         crate::allocator::foundation::Foundation<super::traps::SavedState>,
    _stack:      crate::allocator::foundation::Foundation<super::pages::Stack>,
    context:     Arc<ThreadContext>,
    publication: spin::Once<CompletionKey>,
    name:        ThreadName,
    // Last: the row cannot be prepared again before all actual owners above die.
    target:      super::traps::execution::IdleTarget
}
/// Affine owner of the exact canonical idle publication. The actual store and
/// this holder retain the same core, including its receipt, until machine reset.
/// This grants preparation/install affinity, never permission to release a CPU.
pub(crate) struct SystemIdle
{
    core: Arc<SystemIdleCore>
}
impl SystemIdle
{
    pub(crate) fn target(&self) -> &super::traps::execution::IdleTarget
    {
        &self.core.target
    }

    pub(crate) fn context(&self) -> &Arc<ThreadContext>
    {
        &self.core.context
    }

    pub(crate) fn cpu(&self) -> &super::traps::SavedState
    {
        &self.core.cpu
    }
}
/// Single-use constructor proof. Only the successful native SystemIdle Store
/// publication below can construct this; the same fresh context is embedded in
/// that row. An arbitrary context or a numeric thread ID cannot be supplied.
pub(crate) struct SystemThreadBinding
{
    context:     Arc<ThreadContext>,
    owner:       OwnerRef,
    publication: CompletionKey,
    resources:   crate::kernel::ResourceSelection
}
impl SystemThreadBinding
{
    pub(crate) fn into_parts(
        self)
        -> (Arc<ThreadContext>, OwnerRef, u64, crate::kernel::ResourceSelection)
    {
        (self.context, self.owner, self.publication.id(), self.resources)
    }
}

pub(crate) enum StartError
{
    Clock(super::monitor::Error),
    Wake(super::monitor::WakeError),
    Load(crate::loader::LoadError),
    Idle(super::traps::execution::IdlePreparationFailure),
    Cpus(super::smp::Error)
}
impl core::fmt::Display for StartError
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Clock(cause) => cause.fmt(out),
            Self::Wake(cause) => cause.fmt(out),
            Self::Load(cause) => cause.fmt(out),
            Self::Idle(cause) => cause.fmt(out),
            Self::Cpus(cause) => cause.fmt(out)
        }
    }
}
impl From<mac_formats::Error> for StartError
{
    fn from(cause: mac_formats::Error) -> Self
    {
        Self::Load(cause.into())
    }
}
impl From<crate::loader::LoadError> for StartError
{
    fn from(cause: crate::loader::LoadError) -> Self
    {
        Self::Load(cause)
    }
}

/// Ordinary executors are eligible for automatic placement on the actual
/// core-admitted dispatch set. Today that set contains only the primary; no
/// per-application opt-in is needed when the real service/CPU gates broaden it.
/// Affine System idles remain a distinct sealed Store variant.
#[derive(Clone, Copy)]
pub(crate) enum ThreadAffinity
{
    Eligible,
    Primary
}
impl ThreadAffinity
{
    fn allows(self, hart: HartKey) -> bool
    {
        match self
        {
            Self::Eligible => hart.dispatch_eligible(),
            Self::Primary => hart.is_primary() && hart.dispatch_eligible()
        }
    }
}
#[derive(Clone, Copy)]
struct RunState
{
    // Scheduler-owned execution claim, retained across frame restore and body
    // execution. Only a complete outer-capture handoff may clear it.
    affinity:        ThreadAffinity,
    service:         service::State,
    claimed_by:      Option<HartKey>,
    live:            bool,
    retired:         bool,
    failure_pending: bool,
    failure_parked:  bool,
    switches:        u64,
    waiting:         Option<Wait>,
    woken:           u8,
    ready_since:     Option<u64>,
    last_hart:       Option<HartKey>,
    at_handoff:      bool
}
/// Policy refusal preserves the actual missing/invalid clock or ready interval.
/// This surface does not invent a CPU-time counter.
pub(super) enum RunFactsError
{
    NotReady
    {
        thread: u64
    },
    FutureReady
    {
        thread: u64,
        ready:  u64,
        now:    u64
    },
    Clock(super::monitor::Error)
}
impl RunFactsError
{
    pub(super) fn status(&self) -> mac_abi::Status
    {
        match self
        {
            Self::Clock(cause) => cause.status(),
            Self::NotReady { .. } | Self::FutureReady { .. } => mac_abi::CORRUPT_DATA
        }
    }
}
impl core::fmt::Display for RunFactsError
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::NotReady { thread } =>
            {
                write!(out, "thread {thread} has no actual runnable interval")
            }
            Self::FutureReady { thread,
                                ready,
                                now } =>
            {
                write!(out, "thread {thread} ready timestamp {ready} exceeds round time {now}")
            }
            Self::Clock(cause) => cause.fmt(out)
        }
    }
}
impl RunState
{
    fn requires_primary(self) -> bool
    {
        match self.affinity
        {
            ThreadAffinity::Eligible => self.service.primary(),
            ThreadAffinity::Primary => true
        }
    }

    const fn new() -> Self
    {
        Self { affinity:        ThreadAffinity::Eligible,
               service:         service::State::new(),
               claimed_by:      None,
               live:            false,
               retired:         false,
               failure_pending: false,
               failure_parked:  false,
               switches:        0,
               waiting:         None,
               woken:           0,
               ready_since:     None,
               last_hart:       None,
               at_handoff:      false }
    }

    fn allows(&self, hart: HartKey) -> bool
    {
        self.affinity.allows(hart) && (!self.service.primary() || hart.is_primary())
    }

    fn runnable(&self) -> bool
    {
        self.live && !self.retired && !self.failure_parked && self.waiting.is_none()
    }

    /// Fresh dispatch admission, shared by policy, bootstrap and final commit.
    fn selectable(&self, hart: HartKey, is_current: bool) -> bool
    {
        self.runnable()
        && self.allows(hart)
        && (self.claimed_by.is_none() || (is_current && self.claimed_by == Some(hart)))
    }

    fn ready(&self) -> bool
    {
        self.runnable() && (self.claimed_by.is_none() || self.at_handoff)
    }
}
impl Thread
{
    fn update(&self, change: impl FnOnce(&mut RunState))
    {
        self.update_at(None, change);
    }

    fn update_at(&self, observed: Option<u64>, change: impl FnOnce(&mut RunState))
    {
        let mut state = self.state.get();
        let was_ready = state.ready();
        let was_retired = state.retired;
        let was_failure_parked = state.failure_parked;
        change(&mut state);
        if state.failure_pending && state.service.quiescent()
        {
            state.failure_parked = true;
        }
        if !was_failure_parked && state.failure_parked
        {
            crate::toolbox::processes::termination::progress();
        }
        if !was_retired && state.retired
        {
            if let Some(backing) = self.core.backing.lock().as_ref()
            {
                backing.stack.close();
            }
        }
        if !state.ready()
        {
            state.ready_since = None;
        }
        else if !was_ready
        {
            // Actual publication/activation/unblock/capture edge only. A
            // duplicate wake or unrelated priority change preserves queue age.
            state.ready_since = Some(observed.unwrap_or_else(crate::platform::ticks));
        }
        self.state.set(state);
        if !was_ready && state.ready() && observed.is_none()
        {
            // Capture already owns this round; it is not new queued work.
            // Real event/I/O/activation edges notify only after the first
            // scheduled-frame admission, never to prove a boot self-IPI.
            wake::runnable(state.requires_primary());
        }
    }

    fn capture_runnable(&self, captured: u64)
    {
        // The caller validated the genuine complete outer frame under the
        // scheduler lock. Retaining the claim protects that frame while it
        // competes locally; this metadata bit grants no remote execution.
        self.update_at(Some(captured), |state| state.at_handoff = true);
    }

    fn commit_execution(&self, hart: HartKey)
    {
        self.update(|state| {
                state.at_handoff = false;
                state.last_hart = Some(hart);
                state.claimed_by = Some(hart);
                state.woken = 0;
                state.switches += 1;
            });
    }

    fn policy_record(&self, now_microseconds: u64)
                     -> Result<mac_scheduler_interface::Record, RunFactsError>
    {
        let state = self.state.get();
        let ready_ticks = state.ready_since
                               .filter(|_| state.ready())
                               .ok_or(RunFactsError::NotReady { thread: self.id })?;
        let ready =
            super::monitor::ticks_to_microseconds(ready_ticks).map_err(RunFactsError::Clock)?;
        if ready > now_microseconds
        {
            return Err(RunFactsError::FutureReady { thread: self.id,
                                                    ready,
                                                    now: now_microseconds });
        }
        // Scheduled records are published only by the genuine application or
        // module-worker constructors. Canonical SystemIdle is a distinct Store
        // variant and cannot become a policy candidate. No owner Arc clone or
        // caller-selected program-kind number is needed per candidate.
        let kind = if self.core.context.is_module_worker()
        {
            mac_scheduler_interface::Kind::Module
        }
        else
        {
            mac_scheduler_interface::Kind::Application
        };
        Ok(mac_scheduler_interface::Record { thread: self.id,
                                             ready_since_microseconds: ready,
                                             last_hart: state.last_hart
                                                             .map(HartKey::number)
                                                             .map(|hart| hart as u64),
                                             interactive_wake: state.woken == 2,
                                             kind })
    }
}
#[derive(Clone, Copy)]
enum Wait
{
    Address
    {
        deadline: Option<u64>
    },
    Worker
    {
        deadline: Option<u64>
    },
    Events
    {
        mask: u16, deadline: u64
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
    records:        Option<Store<Record>>,
    waiters:        Vec<EventWaiter>,
    waiters_leased: bool,
    ticks:          u64
}
impl Scheduler
{
    fn thread(&self, slot: usize) -> Option<&Thread>
    {
        self.records.as_ref()?.completing(self.threads.get(slot)?.as_ref()?)?.scheduled()
    }

    fn iter(&self) -> impl Iterator<Item = (usize, &Thread)>
    {
        self.threads.iter().enumerate().filter_map(|(slot, _)| self.thread(slot).map(|t| (slot, t)))
    }

    fn prepare_records(&mut self) -> Result<(), mac_abi::Status>
    {
        if self.records.is_none()
        {
            let (objects, backing) = crate::toolbox::handles::thread_kinds()?;
            self.records = Some(Store::owned(objects, backing).map_err(lifetime::pool_status)?);
        }
        Ok(())
    }

    fn prepare(&mut self) -> Result<usize, mac_abi::Status>
    {
        self.prepare_records()?;
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
        wake::request();
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
    stack:  Arc<stack::StackBacking>,
    loaded: Option<crate::kernel::LoadedImage>
}
struct ThreadCore
{
    address:            Mutex<Option<address::Pending>>,
    backing:            Mutex<Option<ThreadBacking>>,
    launch:             Mutex<Option<crate::drivers::WorkerLaunch>>,
    context:            Arc<ThreadContext>,
    release:            Mutex<Option<ReleasedResource>>,
    binding:            crate::images::Binding,
    top:                usize,
    cleanup_refusal:    Mutex<Option<ReapState>>,
    // Only impossible service binding uncertainty fills this prepaid slot.
    // It intentionally retains the original core/ticket until reset.
    service_quarantine: spin::Once<(Arc<ThreadCore>, mac_abi::Status)>,
    name:               Option<ThreadName>
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
    recovery:   Option<crate::allocator::recovery::Permit>,
    heap:       crate::toolbox::application_heap::RootBinding
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
                      loaded: crate::kernel::LoadedImage, charges: accounting::ThreadCharges,
                      heap: crate::toolbox::application_heap::RootBinding,
                      tls: crate::toolbox::native_threads::RootBinding)
                      -> Result<PreparedThread, PrepareFailure>
{
    let target = crate::allocator::Target::Owner { account: charges.allocation().clone(),
                                                   class:
                                                       mac_kernel_model::policy::MemoryClass::Kernel };
    // The child's genuine account scopes BOTH Arc allocations. Keep loaded
    // borrowed until every fallible preparation has succeeded, so refusal can
    // return the original image use without publishing or losing it.
    let prepared = crate::context::with_allocation(target, || {
        if !owner.accepting()
           || owner.owner().kind() != mac_object_table::domain::DomainKind::Process
           || loaded.binding().owner() != owner.owner().incarnation()
           || heap.owner() != owner.owner()
           || charges.allocation().owner() != owner.owner()
        {
            return Err(mac_abi::ACCESS_DENIED);
        }
        let context = Arc::try_new(ThreadContext::new()).map_err(|_| mac_abi::NO_MEMORY)?;
        let slots = crate::toolbox::native_threads::prepare_slots(&owner, &tls).map_err(|cause| {
                                                                                   cause.status()
                                                                               })?;
        context.install_tls(slots).map_err(|(cause, slots)| {
                                       drop(slots);
                                       cause.status()
                                   })?;
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
        let stack = stack::StackBacking::prepare(&owner, stack)?;
        let core = Arc::try_new(ThreadCore { address: Mutex::new(None),
                                             backing:
                                                 Mutex::new(Some(ThreadBacking { cpu,
                                                                                 stack,
                                                                                 loaded: None })),
                                             launch: Mutex::new(None),
                                             context,
                                             release: Mutex::new(None),
                                             binding: loaded.binding(),
                                             top,
                                             cleanup_refusal: Mutex::new(None),
                                             service_quarantine: spin::Once::new(),
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
                                recovery: None,
                                heap })
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
                               charges: accounting::ThreadCharges, loan: FinderBackingLoan,
                               heap: crate::toolbox::application_heap::RootBinding,
                               tls: crate::toolbox::native_threads::RootBinding)
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
               || heap.owner() != owner.owner()
               || charges.allocation().owner() != owner.owner()
            {
                return Err(mac_abi::ACCESS_DENIED);
            }
            let context = Arc::try_new(ThreadContext::new()).map_err(|_| mac_abi::NO_MEMORY)?;
            let slots =
                crate::toolbox::native_threads::prepare_slots(&owner, &tls).map_err(|cause| {
                                                                               cause.status()
                                                                           })?;
            context.install_tls(slots).map_err(|(cause, slots)| {
                                           drop(slots);
                                           cause.status()
                                       })?;
            context.install_recovery_default(permit.clone());
            let stack_preparation = stack::StackBacking::prepay(&owner)?;
            let core = ThreadCore { address: Mutex::new(None),
                                    backing: Mutex::new(None),
                                    launch: Mutex::new(None),
                                    context,
                                    release: Mutex::new(None),
                                    binding: loaded.binding(),
                                    top: 0,
                                    cleanup_refusal: Mutex::new(None),
                                    service_quarantine: spin::Once::new(),
                                    name: None };
            let core = Arc::try_new(core).map_err(|_| mac_abi::NO_MEMORY)?;
            Ok::<_, mac_abi::Status>((core, stack_preparation))
        })
    });
    let (mut core, stack_preparation) = match metadata
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
    let stack = stack_preparation.finish(stack);
    *storage.backing.get_mut() = Some(ThreadBacking { cpu,
                                                      stack,
                                                      loaded: Some(loaded) });
    Ok(PreparedThread { owner,
                        core: mac_object_table::retained::Read::new(core, context_released),
                        allocation,
                        recovery: Some(permit),
                        heap })
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
    let published = (|| {
        let mut scheduler = SCHEDULER.lock();
        let slot = crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
            scheduler.prepare()
        })?;
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
                Record::Scheduled(Thread { id,
                         core: prepared.core.share(),
                         worker: None, scope: None,
                         state: Cell::new(RunState::new()) })
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
        let stack_binding = prepared.core.backing.lock().as_ref().map_or(Err(mac_abi::BAD_ABI),
                                                                         |backing| {
                                                                             backing.stack
                                                                                    .bind(receipt)
                                                                         });
        let assigned = stack_binding.and_then(|()| {
                                        prepared.core.context.assign(prepared.owner.clone(),
                                                                     id,
                                                                     prepared.core.binding,
                                                                     &prepared.allocation,
                                                                     &prepared.heap)
                                    });
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
    let record = scheduler.records
                          .as_ref()
                          .and_then(|store| store.get(thread))
                          .and_then(Record::scheduled)
                          .ok_or(mac_abi::NOT_FOUND)?;
    if !owner.accepting() || record.core.context.identity().app.0 != owner.owner().incarnation()
    {
        return Err(mac_abi::ACCESS_DENIED);
    }
    if record.state.get().retired || record.state.get().failure_pending
    {
        return Err(mac_abi::SERVICE_BUSY);
    }
    record.update(|state| state.live = true);
    Ok(())
}
pub(crate) fn spawn(owner: OwnerRef, loaded: crate::kernel::LoadedImage,
                    charges: accounting::ThreadCharges,
                    heap: crate::toolbox::application_heap::RootBinding,
                    tls: crate::toolbox::native_threads::RootBinding)
                    -> Result<u64, mac_abi::Status>
{
    let prepared =
        prepare(owner, loaded, charges, heap, tls).map_err(|failure| {
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
    let status = unsafe { function(crate::kernel::services()) };
    if status != mac_abi::OK
    {
        app_return_log(format_args!("APPLICATION_ENTRY_RETURNED entry={entry:#x} \
                                     status={status}\n"));
    }
    // Entry Status describes the CRT service contract, not the program's
    // signed main result. A failed entry aborts with a distinct diagnostic;
    // ordinary main/explicit exit carries its own result through NativeExitCall.
    let mut call =
        mac_abi::processes::NativeExitCall { code:     if status == mac_abi::OK { 0 } else { 101 },
                                             flags:    if status == mac_abi::OK
                                             {
                                                 mac_abi::processes::NATIVE_EXIT_NORMAL
                                             }
                                             else
                                             {
                                                 mac_abi::processes::NATIVE_EXIT_ABORT
                                             },
                                             reserved: 0 };
    let exit_status = unsafe {
        super::traps::gateway(mac_abi::traps::PROCESS,
                              (&mut call as *mut mac_abi::processes::NativeExitCall).cast(),
                              core::mem::size_of_val(&call))
    };
    app_return_log(format_args!("APPLICATION_EXIT_RETURNED entry={entry:#x} \
                                 status={exit_status}; nonreturning System contract lost\n"));
    // EXIT must consume this canonical continuation. Its unexpected return is
    // an unrecoverable platform contract failure, reported by the real trap's
    // loud terminal path. Do not silently retain a spinning app (M-D04).
    unsafe {
        core::arch::asm!("unimp", options(noreturn, nostack));
    }
}

fn app_return_log(arguments: core::fmt::Arguments<'_>)
{
    use core::fmt::Write;
    struct Text
    {
        bytes: [u8; 256],
        used:  usize
    }
    impl Write for Text
    {
        fn write_str(&mut self, text: &str) -> core::fmt::Result
        {
            if text.len() > self.bytes.len() - self.used
            {
                return Err(core::fmt::Error);
            }
            self.bytes[self.used..self.used + text.len()].copy_from_slice(text.as_bytes());
            self.used += text.len();
            Ok(())
        }
    }
    let mut text = Text { bytes: [0; 256],
                          used:  0 };
    if text.write_fmt(arguments).is_err()
    {
        let fallback = b"APPLICATION_RETURN_DIAGNOSTIC_OVERFLOW\n";
        text.bytes[..fallback.len()].copy_from_slice(fallback);
        text.used = fallback.len();
    }
    let mut call = mac_abi::traps::LogCall { bytes:  text.bytes.as_ptr(),
                                             length: text.used };
    // The returning app may be on a peer. The actual LOG trap routes the same
    // continuation before touching fixed-owner firmware; no direct peer UART.
    let _ = unsafe {
        super::traps::gateway(mac_abi::traps::LOG,
                              core::ptr::from_mut(&mut call).cast(),
                              core::mem::size_of_val(&call))
    };
}
fn arm()
{
    arm_ticks(timer_interval());
}
fn arm_ticks(interval: u64)
{
    let deadline = crate::platform::ticks().wrapping_add(interval);
    // Sstc was admitted by firmware before supervisor entry. Enable STIE only
    // with an actual deadline and only when hardware reports it clear. Warm
    // admitted-idle entry enables SSIE alone; this preserves that separate bit.
    unsafe {
        asm!("csrw 0x14d, {deadline}",
             "csrr {enabled}, sie",
             "and {enabled}, {enabled}, {timer}",
             "bnez {enabled}, 2f",
             "csrs sie, {timer}",
             "2:",
             deadline = in(reg) deadline,
             timer = in(reg) 32usize,
             enabled = out(reg) _,
             options(nostack));
    }
}

fn prepare_system_idle(target: super::traps::execution::IdleTarget)
                       -> Result<SystemIdle, mac_abi::Status>
{
    use crate::allocator::{SystemClass, foundation::Foundation};
    crate::context::without_recovery(|| {
        crate::allocator::with_system(SystemClass::Foundation, || {
            let owner = crate::toolbox::handles::system_owner()?;
            let source = crate::kernel::system_resources(&owner)?;
            let resources = source.with(crate::kernel::current_resource_selection)?;
            let stack =
                Foundation::<super::pages::Stack>::allocate(IDLE_STACK_BYTES).map_err(|cause| {
                    serial!("SYSTEM_IDLE_BACKING_REFUSED hart={} resource=stack cause={cause}",
                            target.number());
                    cause.status()
                })?;
            let mut initial = TrapFrame::default();
            initial.pc = idle as *const () as usize;
            initial.status = super::traps::INITIAL_STATUS;
            initial.stack_pointer = stack.top();
            let cpu = Foundation::<super::traps::SavedState>::new(initial).map_err(|cause| {
                          serial!("SYSTEM_IDLE_BACKING_REFUSED hart={} resource=CPU cause={cause}",
                                  target.number());
                          cause.status()
                      })?;
            let context = Arc::try_new(ThreadContext::new()).map_err(|_| mac_abi::NO_MEMORY)?;

            let name = ThreadName::new("System idle")?;
            let core = Arc::try_new(SystemIdleCore { cpu,
                                                     _stack: stack,
                                                     context,
                                                     target,
                                                     publication: spin::Once::new(),
                                                     name }).map_err(|_| mac_abi::NO_MEMORY)?;
            {
                let mut scheduler = SCHEDULER.lock();
                scheduler.prepare_records()?;
                let store = scheduler.records.as_mut().ok_or(mac_abi::BAD_ABI)?;
                let publication = store.insert_with_receipt(&owner, |id| {
                                           Record::SystemIdle(IdleRecord { id,
                                                                           core: core.clone() })
                                       })
                                       .map_err(lifetime::pool_status)?;
                let (_, receipt) = publication.into_parts();
                // No fallible action follows actual native publication. The proof
                // binds this row's same fresh Arc, canonical System owner and source.
                core.publication.call_once(|| receipt);
                ThreadContext::assign_system(SystemThreadBinding { context: core.context
                                                                                .clone(),
                                                                   owner,
                                                                   publication: receipt,
                                                                   resources });
            }
            serial!("SYSTEM_IDLE_REGISTERED thread={} hart={} name={}",
                    core.context.identity().thread.0,
                    core.target.number(),
                    core.name.text());
            Ok(SystemIdle { core })
        })
    })
}

pub(crate) fn start() -> Result<(), StartError>
{
    let interval = super::monitor::microseconds_to_ticks_ceil(QUANTUM_MICROSECONDS)
        .map_err(StartError::Clock)?;
    TIMER_INTERVAL.compare_exchange(0, interval, Ordering::AcqRel, Ordering::Acquire)
                  .map_err(|_| {
                      mac_formats::Error::Malformed("native scheduler timer was already \
                                                     initialized; reset before restarting")
                  })?;
    // Genuine owner/kind services now exist. Registry preparation allocates and
    // publishes each affine idle before we borrow the scheduled-position table.
    // A typed install refusal retains its actual SystemIdle through boot report.
    super::traps::execution::prepare_idles(prepare_system_idle).map_err(StartError::Idle)?;
    // Normal CPU initialization completes real warm admission and a startup
    // interrupt ACK before any scheduled-position lock or author execution.
    // Peers remain AdmittedIdle; this grants no dispatch policy permission.
    super::smp::initialize().map_err(StartError::Cpus)?;
    let wake_receiver = super::monitor::prepare_primary_wake().map_err(StartError::Wake)?;
    let scheduler = SCHEDULER.lock();
    // Module preparations can precede Finder in the shared THRD store. Start
    // the genuine runnable Finder process, never an inert/armed module slot.
    let finder = crate::context::finder_app().0;
    let hart = selected_hart();
    let (slot, t) =
        scheduler.iter()
                 .find(|(_, thread)| {
                     let state = thread.state.get();
                     thread.worker.is_none()
                     && thread.core.context.identity().app.0 == finder
                     && state.selectable(hart, false)
                 })
                 .ok_or(mac_formats::Error::Malformed("scheduler bootstrap has no runnable \
                                                       Finder process"))?;
    let frame =
        t.core
         .cpu_frame()
         .ok_or(mac_formats::Error::Malformed("scheduler bootstrap Finder has no CPU backing"))?;
    if !t.state.get().allows(hart)
    {
        return Err(mac_formats::Error::Malformed("scheduler bootstrap Finder violates actual \
                                                  dispatch eligibility").into());
    }
    if t.state.get().claimed_by.is_some()
    {
        return Err(mac_formats::Error::Malformed("scheduler bootstrap Finder is already claimed").into());
    }
    t.commit_execution(hart);
    t.core.publish_cpu();
    activate_context(Some(&t.core.context));
    select_current(slot);
    drop(scheduler);
    arm();
    // The frame/claim/selection is committed and no scheduler guard survives.
    // Publish the real receiver, then notify only retained callable/cleanup work.
    wake::admitted(wake_receiver);
    hart.resume_activity(false);
    unsafe { __system_start_frame(frame) }
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
pub fn wait_current(mask: u16, timeout_ticks: u32) -> Result<(), mac_abi::Status>
{
    let scheduler = SCHEDULER.lock();
    let thread = scheduler.thread(current()).ok_or(mac_abi::WRONG_THREAD)?;
    service::suspension_allowed(thread.state.get())?;
    if timeout_ticks == 0
    {
        return Ok(());
    }
    let duration = super::monitor::event_ticks_to_ticks(timeout_ticks).map_err(|cause| {
                       if matches!(cause, super::monitor::Error::ClockOverflow)
                       {
                           mac_abi::BAD_ARGUMENT
                       }
                       else
                       {
                           mac_abi::BAD_ABI
                       }
                   })?;
    let deadline = crate::platform::ticks().checked_add(duration).ok_or(mac_abi::BAD_ARGUMENT)?;
    thread.update(|state| {
              state.waiting = Some(Wait::Events { mask,
                                                  deadline });
              state.woken = 0;
          });
    Ok(())
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
    service::suspension_allowed(thread.state.get())?;
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
    if let Some(thread) =
        scheduler.records.as_ref().and_then(|store| store.get(identity)).and_then(Record::scheduled)
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
    if service::defer_retire(thread)
    {
        return;
    }
    if !thread.state.get().retired
    {
        thread.update(|state| {
                  state.live = false;
                  state.retired = true;
                  state.service.retire_committed();
              });
        RETIRED_COUNT.fetch_add(1, Ordering::AcqRel);
    }
    RECLAIM_PENDING.store(true, Ordering::Release);
}
/// Stop execution before dispatching the common ledger walk. Each real thread
/// retains its original job cancellation owner, including after invalidation.
pub(crate) fn retire_owner(owner: u64)
{
    address::cancel_owner(owner);
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
    address::release_retired();
    let mut after = 0;
    loop
    {
        let removed = {
            let mut scheduler = SCHEDULER.lock();
            let found = scheduler.iter().find_map(|(slot, thread)| {
                                            if slot < after
                                               || !thread.state.get().retired
                                               || !thread.state.get().service.quiescent()
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
                                               || thread.core
                                                        .backing
                                                        .lock()
                                                        .as_ref()
                                                        .is_none_or(|backing| {
                                                            Arc::strong_count(&backing.stack) != 1
                                                        })
                                            {
                                                return None;
                                            }
                                            thread.core.release.lock().take().map(|ticket| {
                                                                                 (slot,
                                                                                  thread.id,
                                                                                  ticket,
                                                                                  thread.core
                                                                                        .clone())
                                                                             })
                                        });
            let Some((slot, id, ticket, core)) = found
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
                    let context = core.context.clone();
                    let app = context.identity().app.0;
                    drop(core);
                    Some((removed, context, app, id))
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
        if let Some((removed, context, app, id)) = removed
        {
            context.release();
            drop(context);
            drop(removed);
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
    thread.update(|state| {
              state.claimed_by = None;
              state.at_handoff = false;
          });
    if thread.state.get().retired
    {
        RECLAIM_PENDING.store(true, Ordering::Release);
    }
    worker::execution_released(thread);
    if thread.state.get().failure_pending
    {
        crate::toolbox::processes::termination::progress();
    }
    let released = thread.state.get();
    if released.ready()
    {
        wake::execution_released(hart, released.requires_primary());
    }
}

/// The boot-admitted topology fits the checked native scheduler limits. A
/// draining target remains counted until it commits its own idle receipt.
pub(crate) fn active_harts() -> u32
{
    super::smp::active_harts() as u32
}

/// Lock order for policy placement is SCHEDULER -> original registration Phase
/// -> reset-lifetime admission rows. No provider or Runtime callback occurs.
pub(crate) fn set_policy_harts(pin: &crate::drivers::scheduler::PolicyPin, count: u32)
                               -> Result<(), mac_abi::Status>
{
    super::interrupts::with_masked(|| {
        let scheduler = SCHEDULER.lock();
        let result = crate::drivers::scheduler::with_active_policy(pin, pin.generation(), || {
                         super::smp::apply_harts(pin.generation(), count)
                     }).and_then(core::convert::identity);
        drop(scheduler);
        result
    })
}

/// Withdrawal may be requested only by the real retained policy's retirement
/// path. A true result follows each target's own idle-frame/claim commitment.
pub(crate) fn drain_policy_harts() -> bool
{
    super::interrupts::with_masked(|| {
        let scheduler = SCHEDULER.lock();
        let drained = super::smp::withdraw_harts();
        drop(scheduler);
        drained
    })
}

pub(super) fn schedule(capture: super::traps::OuterCapture) -> *mut TrapFrame
{
    let hart = capture.hart();
    let current = capture.current();
    {
        let scheduler = SCHEDULER.lock();
        validate_capture(&scheduler, &capture);
        if current != IDLE
        {
            if let Some(thread) = scheduler.thread(current)
            {
                thread.capture_runnable(capture.captured_ticks());
            }
        }
    }
    // The admitted hart selection still names the interrupted app until a new CPU frame is
    // selected. Generic scheduler maintenance uses permanent System authority; provider
    // callbacks must explicitly restore their own retained subject selection.
    // Maintenance may enter real owner-affine FILES/ROM/code-publication
    // services. The complete capture carries the authenticated hardware key;
    // allocation or logical app identities cannot impersonate this service hart.
    if super::paging::firmware_owner_hart() == Some(hart.number())
    {
        wake::report_failure();
        super::smp::report_failures();
        crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
            // Device input and cursor publication precede all ordinary maintenance.
            // Never enable nested preemption while a resident manager owns its guards.
            let fresh_input = crate::kernel::poll_waiting_events();
            if INPUT_PRIORITY.background_allowed(fresh_input)
            {
                reclaim_workers();
                reclaim_retired();
                crate::drivers::scheduler::pump();
                crate::drivers::pump_jobs();
                crate::toolbox::files::pump_provider_operations();
                crate::toolbox::runtime::Desktop::pump_standard_file();
                pump_recovery_backing();
                crate::recovery::pump();
                crate::toolbox::crashes::pump_dismissals();
                crate::toolbox::crashes::pump_reports();
                crate::toolbox::processes::documents::pump();
                crate::toolbox::native_threads::pump();
                crate::toolbox::application_heap::wait::pump();
                crate::toolbox::processes::retirement::pump();
                crate::toolbox::processes::finder::pump();
                crate::toolbox::processes::termination::pump();
            }
        });
    }
    let mut scheduler = SCHEDULER.lock();
    validate_capture(&scheduler, &capture);
    scheduler.ticks += 1;
    let now = crate::platform::ticks();
    wake_deadlines(&scheduler, now);
    // Deadline/wake edits above precede Round.now. No scheduler guard or
    // mutable frame borrow may cross foreign policy entry.
    let preferred = if hart.dispatch_eligible() && crate::drivers::scheduler::configured()
    {
        drop(scheduler);
        let attempt = policy::select(&capture);
        if attempt.is_err() && super::paging::firmware_owner_hart() == Some(hart.number())
        {
            // A late result is discarded before any frame/claim change. Input
            // may have arrived during that elapsed interval; service it now.
            crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
                let _ = crate::kernel::poll_waiting_events();
            });
        }
        scheduler = SCHEDULER.lock();
        validate_capture(&scheduler, &capture);
        match attempt
        {
            Ok(preferred) => preferred,
            Err(policy::Expired) =>
            {
                wake_deadlines(&scheduler, crate::platform::ticks());
                None
            }
        }
    }
    else
    {
        None
    };
    // Claim and policy generation are committed in one turn. A snapshot that
    // lost a race is ordinary contention; bootstrap rechecks the same live
    // canonical state instead of quarantining a healthy policy.
    let committed = preferred.as_ref()
                             .and_then(|selection| {
                                 selection.commit(&scheduler, &capture, |slot, quantum| {
                                              commit_choice(&scheduler, &capture, slot, quantum)
                                          })
                                          .ok()
                                          .flatten()
                             })
                             .or_else(|| {
                                 let slot = bootstrap_choice(&scheduler, &capture).unwrap_or(IDLE);
                                 commit_choice(&scheduler, &capture, slot, timer_interval())
                             });
    let Some((next, quantum, idle)) = committed
    else
    {
        claim_lost(hart, current, "fresh canonical frame choice cannot commit");
    };
    // A retained policy Selection owns real Plan/code pins. Never run their
    // destructors while SCHEDULER or the registration phase is held.
    drop(scheduler);
    drop(preferred);
    if hart.dispatch_eligible() && (!idle || hart.is_primary())
    {
        arm_ticks(quantum);
    }
    else
    {
        // Idle peers have no local author quantum. Primary maintenance owns
        // global wait deadlines/input; genuine readiness posts a target wake.
        // A drained peer likewise retains SSIP for admission or shutdown.
        unsafe {
            asm!("csrc sie, {timer}", timer = in(reg) 32usize, options(nostack));
        }
    }
    hart.resume_activity(idle);
    next
}

/// Bounded frame/claim commitment only: no foreign callback, allocation,
/// owning retirement or fallible work follows the first ownership mutation.
fn commit_choice(scheduler: &Scheduler, capture: &super::traps::OuterCapture, index: usize,
                 quantum: u64)
                 -> Option<(*mut TrapFrame, u64, bool)>
{
    let hart = capture.hart();
    let current = capture.current();
    if index != IDLE
    {
        let thread = scheduler.thread(index)?;
        let state = thread.state.get();
        if !state.selectable(hart, index == current)
        {
            return None;
        }
        let next = if index == current { capture.frame() } else { thread.core.cpu_frame()? };
        worker::commit_resume(thread);
        thread.commit_execution(hart);
        thread.core.publish_cpu();
        activate_context(Some(&thread.core.context));
        select_current(index);
        if index != current
        {
            release_claim(scheduler, current, hart);
        }
        return Some((next, quantum, false));
    }
    let next = if current == IDLE { capture.frame() } else { idle_frame() };
    activate_idle_context();
    select_current(IDLE);
    release_claim(scheduler, current, hart);
    super::traps::execution::dispatch::idle_committed(hart, next);
    Some((next, quantum, true))
}

fn wake_deadlines(scheduler: &Scheduler, now: u64)
{
    let invalidated = address::take_invalidations();
    for (_, thread) in scheduler.iter()
    {
        address::observe(thread, now, invalidated);
        if matches!(thread.state.get().waiting,
            Some(Wait::Events { deadline, .. } | Wait::Worker { deadline: Some(deadline) }) if now >= deadline)
        {
            thread.update(|state| state.waiting = None);
            if let Some(worker) = &thread.worker
            {
                let mut state = worker.get();
                state.phase = mac_module::abi::worker::RUNNING;
                worker.set(state);
            }
        }
    }
}

fn bootstrap_choice(scheduler: &Scheduler, capture: &super::traps::OuterCapture) -> Option<usize>
{
    let current = capture.current();
    let hart = capture.hart();
    if hart.is_primary()
    {
        // This fresh choice reads canonical readiness under the existing guard.
        // It can consume earlier runnable hints, unlike revalidating an older
        // immutable foreign-policy Round after a new off-hart wake.
        wake::observed_runnable();
    }
    else
    {
        super::smp::observed_runnable(hart);
    }
    let priority = scheduler.iter()
                            .filter(|(slot, t)| {
                                let state = t.state.get();
                                state.selectable(hart, *slot == current)
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
            if state.selectable(hart, index == current) && (!boosted || state.woken == priority)
            {
                if index == current || thread.core.cpu_frame().is_some()
                {
                    return Some(index);
                }
            }
        }
    }
    None
}

/// Called only after the outer trap's service and frame borrows have ended.
/// A pending failure may still drain genuine Host leases; only the actual
/// quiescent park forbids resuming that author continuation.
pub(crate) fn must_leave_current() -> bool
{
    SCHEDULER.lock().thread(current()).is_some_and(|thread| {
                                          let state = thread.state.get();
                                          state.retired || state.failure_parked
                                      })
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
