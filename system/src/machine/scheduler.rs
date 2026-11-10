//! One-hart timer scheduler. Only the trap entry owns saved CPU frames.
//! Timer interrupts preempt application code; synchronous kernel traps run with
//! interrupts masked, so no allocator/manager guard can be preempted by an app.
use alloc::{sync::Arc, vec::Vec};
use core::{cell::Cell,
           ops::{Deref, DerefMut},
           sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}};

use mac_object_table::{domain::ReleasedResource, store::CompletionKey};
use spinning_top::Spinlock as Mutex;

use crate::{context::{EventWaiter, ThreadContext},
            machine::traps::TrapFrame,
            toolbox::handles::{OwnerRef, Store}};
mod lifetime;
mod admission;
pub(crate) mod application;
pub(crate) mod address;
pub(crate) use address::changed as address_wait_changed;
mod stack;
pub(crate) use stack::{StackPinError, StackWaitPin, pin_application_stack};
mod user;
pub(crate) mod callbacks;
pub(crate) use user::{Fault as UserFault, FaultSnapshot, capture_definition_panic, capture_fault,
                      coroutine_stack, current_user_space, pump_faults};
mod worker;
pub(crate) mod service;
pub(crate) mod wake;
pub(crate) mod pipe;
pub(crate) mod readiness;
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

use crate::machine::traps::execution::{HartKey, IDLE, activate_idle_context, current, idle_frame,
                                       in_io, select_current, selected_hart,
                                       with_system_maintenance};
pub(crate) use crate::machine::traps::execution::{activate_context, authenticate_module,
                                                  critical_active, current_context, io_enter,
                                                  io_leave, module_callback_allowed, module_owner,
                                                  reserve_allocation, reserve_replacement};
// Generated from the same checked platform policy as the linker artifacts.
const STACK_BYTES: usize = crate::machine::memory::APPLICATION_STACK_BYTES as usize;
const IDLE_STACK_BYTES: usize = crate::machine::memory::SYSTEM_STACK_BYTES as usize;
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
        crate::machine::traps::entry_lost("scheduler frame commitment preceded validated \
                                           platform timer initialization");
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
    cpu:         crate::allocator::foundation::Foundation<crate::machine::traps::SavedState>,
    _stack:      crate::allocator::foundation::Foundation<crate::machine::pages::Stack>,
    context:     Arc<ThreadContext>,
    publication: spin::Once<CompletionKey>,
    name:        ThreadName,
    // Last: the row cannot be prepared again before all actual owners above die.
    target:      crate::machine::traps::execution::IdleTarget
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
    pub(crate) fn target(&self) -> &crate::machine::traps::execution::IdleTarget
    {
        &self.core.target
    }

    pub(crate) fn context(&self) -> &Arc<ThreadContext>
    {
        &self.core.context
    }

    pub(crate) fn cpu(&self) -> &crate::machine::traps::SavedState
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
    Idle(crate::machine::traps::execution::IdlePreparationFailure),
    Cpus(crate::machine::smp::Error)
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
    Callback,
    Desktop,
    Address
    {
        deadline: Option<u64>
    },
    NativeJoin
    {
        request: u64
    },
    Sleep
    {
        deadline: u64
    },
    Worker
    {
        deadline: Option<u64>
    },
    Events
    {
        mask:     u16,
        deadline: u64
    },
    Io
    {
        job: crate::context::DeferredIo
    },
    /// Parked on a pipe read or write; the pipe manager wakes this thread after
    /// changing the ring or an endpoint count.
    Pipe
    {
        pipe:     u64,
        deadline: Option<u64>
    },
    Child
    {
        child: u64
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
}
static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler { records:        None,
                                                            threads:        Vec::new(),
                                                            waiters:        Vec::new(),
                                                            waiters_leased: false,
                                                            ticks:          0 });
pub(crate) fn initialize_records() -> Result<(), mac_abi::Status>
{
    if !crate::context::current_thread_context().is_bootstrap()
    {
        return Err(mac_abi::ACCESS_DENIED);
    }
    let (objects, backing) = crate::toolbox::handles::thread_kinds()?;
    let mut prepared =
        Some(crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
                 Store::owned_prepared(objects, backing).map_err(lifetime::pool_status)
             })?);
    let result = super::interrupts::with_masked(|| {
        let mut scheduler = SCHEDULER.lock();
        if scheduler.records.is_some()
        {
            return Err(mac_abi::BAD_ABI);
        }
        scheduler.records = prepared.take();
        Ok(())
    });
    drop(prepared);
    result
}
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
    user::faults_changed();
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
    cpu:   accounting::Charged<crate::machine::traps::SavedState>,
    stack: Arc<stack::StackBacking>,
    image: Option<ApplicationImageUse>
}
enum ApplicationImageUse
{
    // These guards retain executable mappings until the thread backing is dropped.
    Main
    {
        _image: crate::kernel::LoadedImage
    },
    Child
    {
        _execution: crate::images::ApplicationExecution
    }
}
struct ThreadCore
{
    address:            Mutex<Option<address::Pending>>,
    backing:            Mutex<Option<ThreadBacking>>,
    launch:             Mutex<Option<crate::drivers::WorkerLaunch>>,
    context:            Arc<ThreadContext>,
    release:            Mutex<Option<ReleasedResource>>,
    binding:            crate::images::Binding,
    space:              Option<crate::machine::paging::process::Pin>,
    top:                usize,
    cleanup_refusal:    Mutex<Option<ReapState>>,
    // Only impossible service binding uncertainty fills this prepaid slot.
    // It intentionally retains the original core/ticket until reset.
    service_quarantine: spin::Once<(Arc<ThreadCore>, mac_abi::Status)>,
    user_fault:         Mutex<Option<user::Fault>>,
    runtime:            Option<Arc<user::Runtime>>,
    callback_queue:     Option<Arc<crate::toolbox::resources::definitions::callbacks::Queue>>,
    name:               Mutex<Option<ThreadName>>,
    application:        Option<Arc<application::Completion>>,
    pending_join:       Mutex<Option<application::PendingJoin>>,
    pending_pipe:       Mutex<Option<pipe::Pending>>,
    readiness:          Mutex<readiness::State>
}
impl ThreadCore
{
    fn publish_user_backing(&self) -> Result<(), mac_abi::Status>
    {
        user::retirement::publish(self)
    }

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
            crate::machine::emergency::enter(crate::emergency::Kind::SystemPanic,
                                             format_args!("Claimed thread lost CPU backing \
                                                           before frame publication."));
        };
        crate::machine::traps::publish_cpu(&backing.cpu);
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
#[derive(Clone, Copy)]
pub(crate) enum SpawnRefusal
{
    RetryableAdmission,
    TerminalBinding,
    Cancelled
}
pub(crate) struct SpawnFailure
{
    pub(crate) disposition: SpawnRefusal,
    pub(crate) status:      mac_abi::Status,
    pub(crate) prepared:    PreparedThread
}
pub(crate) fn prepare(owner: crate::toolbox::handles::OwnerRef,
                      loaded: crate::kernel::LoadedImage, charges: accounting::ThreadCharges,
                      heap: crate::toolbox::application_heap::RootBinding,
                      tls: crate::toolbox::native_threads::RootBinding,
                      space: crate::machine::paging::process::Pin)
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
           || !space.matches(&owner, loaded.binding())
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
        let runtime = user::Runtime::prepare(&owner, &loaded, &space)?;
        let (stack, mut cpu, allocation) = charges.allocate(&owner, |_| TrapFrame::default())
                                                  .map_err(|cause| {
                                                      serial!("THREAD_BACKING_REFUSED: {cause}");
                                                      cause.status()
                                                  })?;
        let stack = stack::StackBacking::prepare_user(&owner, stack, &space)?;
        let top = stack.user_top()?;
        let mut initial = TrapFrame::default();
        initial.pc = loaded.entry();
        initial.status = crate::machine::trap_frame::USER_STATUS;
        initial.stack_pointer = top;
        crate::machine::trap_frame::set_result(&mut initial, runtime.descriptor());
        cpu.initialize(initial).map_err(|cause| cause.status())?;
        let core = Arc::try_new(ThreadCore { address: Mutex::new(None),
                                             backing:
                                                 Mutex::new(Some(ThreadBacking { cpu,
                                                                                 stack,
                                                                                 image: None })),
                                             launch: Mutex::new(None),
                                             context,
                                             release: Mutex::new(None),
                                             binding: loaded.binding(),
                                             space: Some(space),
                                             top,
                                             cleanup_refusal: Mutex::new(None),
                                             service_quarantine: spin::Once::new(),
                                             user_fault: Mutex::new(None),
                                             runtime: Some(runtime),
                                             callback_queue: None,
                                             name: Mutex::new(None),
                                             application: None,
                                             pending_join: Mutex::new(None),
                                             pending_pipe: Mutex::new(None), readiness: Mutex::new(readiness::State::default()) }).map_err(|_| {
                                                                                  mac_abi::NO_MEMORY
                                                                              })?;
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
                Some(backing) => backing.image = Some(ApplicationImageUse::Main { _image: loaded }),
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
    pub(crate) loan:   Option<FinderBackingLoan>
}
/// The same process entry, CPU frame and THRD publication use boot-held native
/// backing. A refused post-loan mapping/metadata preparation returns its real
/// backing through owned retirement; it never fabricates a restored loan.
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
            let space =
                crate::machine::paging::process::admit_recovery(&owner,
                                                                charges.allocation(),
                                                                &loaded,
                                                                permit.clone()).map_err(|cause| {
                    serial!("PROCESS_SPACE_REFUSED owner={} {cause}", owner.owner().incarnation());
                    cause.status()
                })?;
            let runtime = user::Runtime::prepare(&owner, &loaded, &space)?;
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
                                    space: Some(space),
                                    top: 0,
                                    cleanup_refusal: Mutex::new(None),
                                    service_quarantine: spin::Once::new(),
                                    user_fault: Mutex::new(None),
                                    runtime: Some(runtime),
                                    callback_queue: None,
                                    name: Mutex::new(None),
                                    application: None,
                                    pending_join: Mutex::new(None),
                                    pending_pipe: Mutex::new(None), readiness: Mutex::new(readiness::State::default()) };
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
                                                loan: Some(loan) });
        }
    };
    let Some(storage) = Arc::get_mut(&mut core)
    else
    {
        return Err(RecoveryPrepareFailure { status: mac_abi::BAD_ABI,
                                            loaded,
                                            loan: Some(loan) });
    };
    let backing = charges.allocate_recovery(&owner, loan, |_| TrapFrame::default());
    let (stack, mut cpu, allocation) = match backing
    {
        Ok(backing) => backing,
        Err((cause, loan)) =>
        {
            return Err(RecoveryPrepareFailure { status: cause.status(),
                                                loaded,
                                                loan: Some(loan) });
        }
    };
    let prepared = (|| -> Result<_, mac_abi::Status> {
        let space = storage.space.as_ref().ok_or(mac_abi::BAD_ABI)?;
        let runtime = storage.runtime.as_ref().ok_or(mac_abi::BAD_ABI)?;
        let stack = stack_preparation.finish(stack, Some(space))?;
        let top = stack.user_top()?;
        let mut frame = TrapFrame::default();
        frame.pc = loaded.entry();
        frame.status = crate::machine::trap_frame::USER_STATUS;
        frame.stack_pointer = top;
        crate::machine::trap_frame::set_result(&mut frame, runtime.descriptor());
        cpu.initialize(frame).map_err(|cause| cause.status())?;
        Ok((stack, top))
    })();
    let (stack, top) = match prepared
    {
        Ok(value) => value,
        Err(status) =>
        {
            return Err(RecoveryPrepareFailure { status,
                                                loaded,
                                                loan: None });
        }
    };
    storage.top = top;
    *storage.backing.get_mut() =
        Some(ThreadBacking { cpu,
                             stack,
                             image: Some(ApplicationImageUse::Main { _image: loaded }) });
    Ok(PreparedThread { owner,
                        core: mac_object_table::retained::Read::new(core, context_released),
                        allocation,
                        recovery: Some(permit),
                        heap })
}

pub(crate) fn spawn_prepared(prepared: PreparedThread, runnable: bool)
                             -> Result<u64, SpawnFailure>
{
    let mut disposition = SpawnRefusal::RetryableAdmission;
    // Publication creates an inert canonical THRD first. No hart may select
    // user code until both actual stack and descriptor PTE ACKs are complete.
    let published = match admission::publish(&prepared, false)
    {
        Err(cause) =>
        {
            serial!("THREAD_ADMISSION_REFUSED owner={} {cause}",
                    prepared.owner.owner().incarnation());
            Err(cause.status())
        }
        Ok(publication) =>
        {
            match publication.assigned
            {
                Ok(()) =>
                {
                    match prepared.core.publish_user_backing().and_then(|()| {
                        if runnable { start_prepared(publication.id, &prepared.owner) }
                        else { Ok(()) }
                    })
                    {
                        Ok(()) => Ok(publication.id),
                        Err(status) =>
                        {
                            disposition = SpawnRefusal::TerminalBinding;
                            let scheduler = SCHEDULER.lock();
                            if let Some((_, thread)) = scheduler.iter()
                                .find(|(_, thread)| thread.id == publication.id)
                            {
                                retire(thread);
                            }
                            Err(status)
                        }
                    }
                }
                Err(status) =>
                {
                    disposition = SpawnRefusal::TerminalBinding;
                    let removed = super::interrupts::with_masked(|| {
                        let mut scheduler = SCHEDULER.lock();
                        let slot = scheduler.iter().find_map(|(slot, thread)| {
                                                       (thread.id == publication.id).then_some(slot)
                                                   });
                        let removed =
                            scheduler.records
                                     .as_mut()
                                     .and_then(|store| store.remove_with_backing(publication.id));
                        if let Some(slot) = slot
                        {
                            if removed.is_some()
                            {
                                scheduler.threads[slot] = None;
                            }
                            else if let Some(thread) = scheduler.thread(slot)
                            {
                                retire(thread);
                            }
                        }
                        removed
                    });
                    // Includes detached canonical owner capacity, not only Record.
                    // A concurrent walk may already own the row; leave it inert and
                    // retiring, retaining the same original preparation on refusal.
                    drop(removed);
                    Err(status)
                }
            }
        }
    };
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
                               disposition: if prepared.owner.accepting()
                               {
                                   disposition
                               }
                               else
                               {
                                   SpawnRefusal::Cancelled
                               },
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
                    tls: crate::toolbox::native_threads::RootBinding,
                    space: crate::machine::paging::process::Pin, runnable: bool)
                    -> Result<u64, mac_abi::Status>
{
    let prepared = prepare(owner, loaded, charges, heap, tls, space).map_err(|failure| {
                       let PrepareFailure { status,
                                            loaded } = failure;
                       // Failed preparation retains the unique image
                       // until its caller accepts
                       // the refusal; dispose that lease through its
                       // ordinary owned path.
                       drop(loaded);
                       status
                   })?;
    spawn_prepared(prepared, runnable).map_err(|failure| failure.status)
}

fn arm()
{
    arm_ticks(timer_interval());
}
fn arm_ticks(interval: u64)
{
    let deadline = crate::platform::ticks().wrapping_add(interval);
    // The timer is a CPU primitive: programming a deadline and enabling its
    // interrupt differ per architecture, the scheduling policy above does not.
    super::cpu::set_timer_deadline(deadline);
}

fn prepare_system_idle(target: crate::machine::traps::execution::IdleTarget)
                       -> Result<SystemIdle, mac_abi::Status>
{
    use crate::allocator::{SystemClass, foundation::Foundation};
    crate::context::without_recovery(|| {
        crate::allocator::with_system(SystemClass::Foundation, || {
            let owner = crate::toolbox::handles::system_owner()?;
            let source = crate::kernel::system_resources(&owner)?;
            let resources = source.with(crate::kernel::current_resource_selection)?;
            let stack =
                Foundation::<crate::machine::pages::Stack>::allocate(IDLE_STACK_BYTES).map_err(|cause| {
                    serial!("SYSTEM_IDLE_BACKING_REFUSED hart={} resource=stack cause={cause}",
                            target.number());
                    cause.status()
                })?;
            let mut initial = TrapFrame::default();
            initial.pc = idle as *const () as usize;
            initial.status = crate::machine::trap_frame::INITIAL_STATUS;
            initial.stack_pointer = stack.top();
            let cpu =
                Foundation::<crate::machine::traps::SavedState>::new(initial).map_err(|cause| {
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
            let mut permit = admission::thread_permit(&owner).map_err(|cause| {
                                 serial!("SYSTEM_IDLE_PREPARATION_REFUSED hart={} {cause}",
                                         core.target.number());
                                 cause.status()
                             })?;
            let mut record = Record::SystemIdle(IdleRecord { id:   permit.id(),
                                                             core: core.clone() });
            loop
            {
                let result =
                    super::interrupts::with_masked(|| {
                        let mut scheduler = SCHEDULER.lock();
                        let published = match scheduler.records.as_mut()
                        {
                            Some(store) => store.publish_prepared(permit, record)
                                                .map_err(|failure| {
                                                    (admission::Error::Publication(failure.cause),
                                                     failure.permit,
                                                     failure.value)
                                                }),
                            None =>
                            {
                                Err((admission::Error::Admission { status: mac_abi::BAD_ABI,
                                                                   stage:  "canonical System idle \
                                                                            THRD Store" },
                                     permit,
                                     record))
                            }
                        };
                        match published
                        {
                            Err(failure) => Err(failure),
                            Ok((publication, old)) =>
                            {
                                let (_, receipt) = publication.into_parts();
                                // Only this successful canonical birth constructs the
                                // sealed System binding. Idle never enters the run queue.
                                core.publication.call_once(|| receipt);
                                ThreadContext::assign_system(SystemThreadBinding {
                                    context: core.context.clone(),
                                    owner: owner.clone(),
                                    publication: receipt,
                                    resources
                                });
                                Ok(old)
                            }
                        }
                    });
                match result
                {
                    Ok(old) =>
                    {
                        old.dispose();
                        break;
                    }
                    Err((cause, returned_permit, returned_record)) =>
                    {
                        permit = returned_permit;
                        record = returned_record;
                        if !matches!(cause, admission::Error::Publication(
                            mac_object_table::store::PreparedError::CapacityChanged))
                        {
                            serial!("SYSTEM_IDLE_PUBLICATION_REFUSED hart={} {cause}",
                                    core.target.number());
                            return Err(cause.status());
                        }
                    }
                }
            }
            serial!("SYSTEM_IDLE_REGISTERED thread={} hart={} name={}",
                    core.context.identity().thread.0,
                    core.target.number(),
                    core.name.text());
            Ok(SystemIdle { core })
        })
    })
}

pub(crate) fn start(pending: Option<crate::toolbox::processes::startup::PendingStartup>)
                    -> Result<(), StartError>
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
    crate::machine::traps::execution::prepare_idles(prepare_system_idle).map_err(StartError::Idle)?;
    // Normal CPU initialization completes real warm admission and a startup
    // interrupt ACK before any scheduled-position lock or author execution.
    // Peers remain AdmittedIdle; this grants no dispatch policy permission.
    crate::machine::smp::initialize().map_err(StartError::Cpus)?;
    let wake_receiver = super::monitor::prepare_primary_wake().map_err(StartError::Wake)?;
    if let Some(token) = pending
    {
        // Only the actual installed initial Finder flight authorizes this
        // route. An absent runnable Finder alone must remain a boot error.
        if !crate::toolbox::processes::startup::pending(&token)
        {
            return Err(mac_formats::Error::Malformed("scheduler startup token has no retained \
                                                      initial Finder readiness flight").into());
        }
        let hart = selected_hart();
        let frame = idle_frame();
        crate::machine::traps::execution::select_space(None);
        activate_idle_context();
        select_current(IDLE);
        crate::machine::traps::execution::dispatch::idle_committed(hart, frame);
        // StartupFlight owns all real admission/job/image/boot-panel resources.
        // This sealed, non-Clone token owns no account pin and must not survive
        // forever on the abandoned bootstrap stack.
        drop(token);
        arm();
        wake::admitted(wake_receiver);
        hart.resume_activity(true);
        unsafe {
            crate::machine::paging::activate_scheduled_root();
            __system_start_frame(frame)
        }
    }
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
    select_space(&t.core);
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
    // The genuine selected claim pins its SPCE throughout restoration.
    unsafe {
        crate::machine::paging::activate_scheduled_root();
        __system_start_frame(frame)
    }
}
extern "C" fn idle() -> !
{
    loop
    {
        // No application is runnable. Only this dedicated stack executes the
        // wait; an application can never suspend while holding a resident guard.
        super::cpu::wait_for_interrupt();
    }
}

/// Called only by the outermost native wait trap, with interrupts masked.
pub fn wait_current(mask: u16, timeout_ticks: u32) -> Result<(), mac_abi::Status>
{
    let scheduler = SCHEDULER.lock();
    let thread = scheduler.thread(current()).ok_or(mac_abi::WRONG_THREAD)?;
    service::suspension_allowed(thread.state.get())?;
    readiness::disarm(thread);
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
        crate::context::DeferredIo::FileOperation(handle) =>
        {
            crate::toolbox::files::local::pending::io_pending(thread.core
                                                                    .context
                                                                    .identity()
                                                                    .thread,
                                                              handle)?
        }
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
/// The Process Manager holds the result condition until the wait is armed.
pub(crate) fn park_child(child: u64, suspension: Result<(), mac_abi::Status>)
                         -> Result<(), mac_abi::Status>
{
    suspension?;
    let scheduler = SCHEDULER.lock();
    let thread = scheduler.thread(current()).ok_or(mac_abi::WRONG_THREAD)?;
    service::suspension_allowed(thread.state.get())?;
    thread.update(|state| {
              state.waiting = Some(Wait::Child { child });
              state.woken = 0;
          });
    Ok(())
}

pub(crate) fn wake_child(child: u64)
{
    let scheduler = SCHEDULER.lock();
    readiness::advance();
    for (_, thread) in scheduler.iter()
    {
        readiness::wake_matching(thread,mac_abi::readiness::CHILD,child);
        if thread.state.get().live
           && matches!(thread.state.get().waiting, Some(Wait::Child { child: id }) if id == child)
        {
            thread.update(|state| {
                      state.waiting = None;
                      state.woken = 1;
                  });
            wake::runnable(thread.state.get().requires_primary());
        }
    }
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
        if let Some(completion) = thread.core.application.as_ref()
        {
            completion.retire();
        }
        RETIRED_COUNT.fetch_add(1, Ordering::AcqRel);
    }
    RECLAIM_PENDING.store(true, Ordering::Release);
}
/// Stop execution before dispatching the common ledger walk. Each real thread
/// retains its original job cancellation owner, including after invalidation.
pub(crate) fn retire_owner(owner: u64)
{
    address::cancel_owner(owner);
    pipe::cancel_owner(owner);
    readiness::cancel_owner(owner);
    application::cancel_owner(owner);
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
    user::retirement::reclaim();
}

fn claim_lost(hart: HartKey, slot: usize, reason: &str) -> !
{
    crate::machine::emergency::enter(crate::emergency::Kind::SystemPanic,
                                     format_args!("Scheduler execution claim invariant: hart \
                                                   {}, slot {}, {}.",
                                                  hart.number(),
                                                  slot,
                                                  reason));
}
fn validate_capture(scheduler: &Scheduler, capture: &crate::machine::traps::OuterCapture)
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
    application::execution_released(&scheduler, thread);
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
    crate::machine::smp::active_harts() as u32
}

/// Lock order for policy placement is SCHEDULER -> original registration Phase
/// -> reset-lifetime admission rows. No provider or Runtime callback occurs.
pub(crate) fn set_policy_harts(pin: &crate::drivers::scheduler::PolicyPin, count: u32)
                               -> Result<(), mac_abi::Status>
{
    super::interrupts::with_masked(|| {
        let scheduler = SCHEDULER.lock();
        let result = crate::drivers::scheduler::with_active_policy(pin, pin.generation(), || {
                         crate::machine::smp::apply_harts(pin.generation(), count)
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
        let drained = crate::machine::smp::withdraw_harts();
        drop(scheduler);
        drained
    })
}

pub(super) fn schedule(capture: crate::machine::traps::OuterCapture) -> *mut TrapFrame
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
    // selected. A captured logical scope borrows the canonical System invocation; provider
    // callbacks must explicitly restore their own retained subject selection.
    // Maintenance may enter real owner-affine FILES/ROM/code-publication
    // services. The complete capture carries the authenticated hardware key;
    // allocation or logical app identities cannot impersonate this service hart.
    if crate::machine::paging::firmware_owner_hart() == Some(hart.number())
    {
        with_system_maintenance(&capture, || {
            wake::report_failure();
            crate::machine::smp::report_failures();
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
                    crate::toolbox::processes::startup::pump();
                    crate::toolbox::files::pump_provider_operations();
                    crate::toolbox::files::installed_fonts::pump();
                    crate::toolbox::files::applications::pump();
                    crate::toolbox::files::provider_icons::pump();
                    crate::toolbox::runtime::Toolbox::pump_standard_file();
                    pump_recovery_backing();
                    crate::recovery::pump();
                    crate::toolbox::crashes::pump_dismissals();
                    crate::toolbox::crashes::pump_reports();
                    crate::toolbox::processes::documents::pump();
                    crate::toolbox::native_threads::pump();
                    crate::toolbox::application_heap::wait::pump();
                    crate::kernel::pump_module_images();
                    crate::toolbox::processes::retirement::pump();
                    crate::toolbox::processes::finder::pump();
                    crate::toolbox::processes::termination::pump();
                    application::pump_rights();
                }
            });
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
        if attempt.is_err() && crate::machine::paging::firmware_owner_hart() == Some(hart.number())
        {
            // A late result is discarded before any frame/claim change. Input
            // may have arrived during that elapsed interval; service it now.
            with_system_maintenance(&capture, || {
                crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
                    let _ = crate::kernel::poll_waiting_events();
                });
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
        // A drained peer likewise retains its admission interrupt source.
        super::cpu::disable_timer_interrupt();
    }
    hart.resume_activity(idle);
    next
}

/// Bounded frame/claim commitment only: no foreign callback, allocation,
/// owning retirement or fallible work follows the first ownership mutation.
fn commit_choice(scheduler: &Scheduler, capture: &crate::machine::traps::OuterCapture,
                 index: usize, quantum: u64)
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
        select_space(&thread.core);
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
    crate::machine::traps::execution::select_space(None);
    select_current(IDLE);
    release_claim(scheduler, current, hart);
    crate::machine::traps::execution::dispatch::idle_committed(hart, next);
    Some((next, quantum, true))
}

fn wake_deadlines(scheduler: &Scheduler, now: u64)
{
    let invalidated = address::take_invalidations();
    for (_, thread) in scheduler.iter()
    {
        address::observe(thread, now, invalidated);
        if matches!(thread.state.get().waiting,
            Some(Wait::Events { deadline, .. } | Wait::Worker { deadline: Some(deadline) } | Wait::Pipe { deadline: Some(deadline), .. } | Wait::Sleep { deadline }) if now >= deadline)
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

fn bootstrap_choice(scheduler: &Scheduler, capture: &crate::machine::traps::OuterCapture)
                    -> Option<usize>
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
        crate::machine::smp::observed_runnable(hart);
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

/// The actual SCHEDULER turn pins this space through the outgoing claim.
/// Final native return reads the selected hart's scalar without another lock.
fn select_space(core: &ThreadCore)
{
    if core.context.is_module_worker()
    {
        if core.space.is_some()
        {
            crate::machine::traps::entry_lost("module THRD carries a Process space")
        }
        crate::machine::traps::execution::select_space(None);
    }
    else
    {
        let Some(space) = core.space.as_ref()
        else
        {
            crate::machine::traps::entry_lost("application THRD has no canonical Process space")
        };
        if space.owner().incarnation() != core.binding.owner()
        {
            crate::machine::traps::entry_lost("application THRD Process space owner changed")
        }
        crate::machine::traps::execution::select_space(Some(space));
    }
}

fn application_space(executor: &crate::context::ApplicationExecutor<'_>)
                     -> Result<crate::machine::paging::process::Pin, mac_abi::Status>
{
    let scheduler = SCHEDULER.lock();
    let thread = scheduler.iter()
                          .find_map(|(_, t)| (t.id == executor.thread()).then_some(t))
                          .ok_or(mac_abi::WRONG_THREAD)?;
    let space = thread.core.space.as_ref().ok_or(mac_abi::ACCESS_DENIED)?;
    if !space.matches(executor.owner(), executor.image())
    {
        return Err(mac_abi::ACCESS_DENIED);
    }
    Ok(space.clone())
}
