//! RV64 one-hart timer scheduler. Only this adapter owns saved CPU frames.
//! Timer interrupts preempt application code; synchronous kernel traps run with
//! interrupts masked, so no allocator/manager guard can be preempted by an app.
use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
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
pub(crate) use lifetime::{ReleaseThreads, specification};
const IDLE: usize = usize::MAX;
const STACK_WORDS: usize = 16384; // 256KiB, including nested service/definition frames.
// The CLINT runs at 10 MHz. At 500 us, two runnable peers can consume the
// entire 1 ms interaction budget before a queued caller resumes. Use 250 us
// for this resident round-robin policy; a freshly woken input consumer still
// takes priority. This is not a hard real-time or future scheduler contract.
const TIMER_INTERVAL: u64 = 2_500;
static ACTIVE: Mutex<Option<Arc<ThreadContext>>> = Mutex::new(None);
static CURRENT: AtomicUsize = AtomicUsize::new(IDLE);
static IO: AtomicBool = AtomicBool::new(false);
static RECLAIM_PENDING: AtomicBool = AtomicBool::new(false);
static RETIRED_COUNT: AtomicUsize = AtomicUsize::new(0);
static INPUT_PRIORITY: crate::toolbox::events::priority::InputPriority =
    crate::toolbox::events::priority::InputPriority::new();
struct Thread
{
    id:    u64,
    core:  Arc<ThreadCore>,
    state: Cell<RunState>
}
#[derive(Clone, Copy)]
struct RunState
{
    frame:    usize,
    live:     bool,
    retired:  bool,
    switches: u64,
    waiting:  Option<Wait>,
    woken:    u8
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
struct Idle
{
    _stack: Box<[u128]>,
    frame:  usize
}
#[derive(Clone, Copy)]
enum Wait
{
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
    records:        Option<Store<Thread>>,
    waiters:        Vec<EventWaiter>,
    waiters_leased: bool,
    idle:           Option<Idle>,
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
                                                            idle:           None,
                                                            ticks:          0 });
unsafe extern "C" {
    fn __system_start_frame(frame: *mut TrapFrame) -> !;
}
pub fn current_context() -> crate::context::ContextRef
{
    if IO.load(Ordering::Relaxed)
    {
        return crate::context::ContextRef::Bootstrap;
    }
    super::interrupts::with_masked(|| {
        ACTIVE.lock()
              .as_ref()
              .cloned()
              .map(crate::context::ContextRef::thread)
              .unwrap_or(crate::context::ContextRef::Bootstrap)
    })
}
// Scheduler -> ACTIVE is the only held-lock ordering. Lookup releases ACTIVE
// before any caller runs, including completion-before-park under scheduler lock.
fn activate_context(context: Option<&Arc<ThreadContext>>)
{
    let mut active = ACTIVE.lock();
    let unchanged = match (active.as_ref(), context)
    {
        (Some(current), Some(next)) => Arc::ptr_eq(current, next),
        (None, None) => true,
        _ => false
    };
    if !unchanged
    {
        *active = context.cloned();
    }
}
pub(crate) fn context_released()
{
    if RETIRED_COUNT.load(Ordering::Acquire) != 0
    {
        RECLAIM_PENDING.store(true, Ordering::Release);
    }
}
pub fn io_enter() -> bool
{
    IO.swap(true, Ordering::Relaxed)
}
pub fn io_leave(previous: bool)
{
    IO.store(previous, Ordering::Relaxed);
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
struct ThreadCore
{
    // Actual stack disposal precedes releasing the image's execution/source pin.
    _stack:  Box<[u128]>,
    _loaded: Option<crate::kernel::LoadedImage>,
    context: Arc<ThreadContext>,
    release: Mutex<Option<ReleasedResource>>,
    binding: crate::images::Binding,
    frame:   usize,
    top:     usize
}
pub(crate) struct PreparedThread
{
    owner: crate::toolbox::handles::OwnerRef,
    core:  mac_object_table::retained::Read<ThreadCore>
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
pub(crate) fn prepare(owner: crate::toolbox::handles::OwnerRef, loaded: crate::kernel::LoadedImage)
                      -> Result<PreparedThread, PrepareFailure>
{
    let prepared = (|| {
        if !owner.accepting() || loaded.binding().owner() != owner.owner().incarnation()
        {
            return Err(mac_abi::ACCESS_DENIED);
        }
        let context = Arc::try_new(ThreadContext::new()).map_err(|_| mac_abi::NO_MEMORY)?;
        let context_bytes = super::traps::frame_reservation_bytes();
        if context_bytes >= STACK_WORDS * core::mem::size_of::<u128>()
        {
            serial!("PROCESS_CONTEXT_REFUSED: {} bytes of CPU state do not fit the configured {} \
                     byte thread stack",
                    context_bytes,
                    STACK_WORDS * core::mem::size_of::<u128>());
            return Err(mac_abi::NO_MEMORY);
        }
        let mut storage = alloc::vec::Vec::<u128>::new();
        storage.try_reserve_exact(STACK_WORDS).map_err(|_| mac_abi::NO_MEMORY)?;
        storage.resize(STACK_WORDS, 0);
        let mut stack = storage.into_boxed_slice();
        // SAFETY: the exclusive owned allocation includes the validated frame
        // reservation and remains pinned at this address until thread reaping.
        let top = unsafe { stack.as_mut_ptr().add(stack.len()) } as usize;
        let frame = (top - core::mem::size_of::<TrapFrame>()) as *mut TrapFrame;
        let mut initial = TrapFrame::default();
        initial.pc = app_start as *const () as usize;
        initial.status = super::traps::INITIAL_STATUS;
        initial.registers[8] = loaded.entry();
        // gp is unused by the freestanding PC-relative executable contract.
        unsafe {
            frame.write(initial);
        }
        Ok((context, stack, frame as usize, top))
    })();
    match prepared
    {
        Ok((context, stack, frame, top)) =>
        {
            let binding = loaded.binding();
            let mut core = match Arc::try_new(ThreadCore { _stack: stack,
                                                           _loaded: None,
                                                           context,
                                                           release: Mutex::new(None),
                                                           binding,
                                                           frame,
                                                           top })
            {
                Ok(core) => core,
                Err(_) =>
                {
                    return Err(PrepareFailure { status: mac_abi::NO_MEMORY,
                                                loaded });
                }
            };
            let Some(storage) = Arc::get_mut(&mut core)
            else
            {
                serial!("THREAD_CORE_REFUSED: freshly allocated preparation is not exclusive");
                return Err(PrepareFailure { status: mac_abi::BAD_ABI,
                                            loaded });
            };
            storage._loaded = Some(loaded);
            Ok(PreparedThread { owner,
                                core: mac_object_table::retained::Read::new(core,
                                                                            context_released) })
        }
        Err(status) =>
        {
            Err(PrepareFailure { status,
                                 loaded })
        }
    }
}
pub(crate) fn spawn_prepared(prepared: PreparedThread, runnable: bool)
                             -> Result<u64, SpawnFailure>
{
    let published = (|| {
        let mut scheduler = SCHEDULER.lock();
        let slot = scheduler.prepare()?;
        if !prepared.owner.accepting()
        {
            return Err(mac_abi::ACCESS_DENIED);
        }
        let store = scheduler.records.as_mut().ok_or(mac_abi::BAD_ABI)?;
        let publication = store.insert_with_receipt(&prepared.owner, |id| {
                                   Thread { id,
                                            core: prepared.core.share(),

                                            state: Cell::new(RunState { frame:    prepared.core
                                                                                          .frame,
                                                                        live:     false,
                                                                        retired:  false,
                                                                        switches: 0,
                                                                        waiting:  None,
                                                                        woken:    0 }) }
                               })
                               .map_err(lifetime::pool_status)?;
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
        let assigned =
            prepared.core.context.assign(prepared.owner.clone(), id, prepared.core.binding);
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
            serial!("PROCESS_THREAD app={} thread={} stack=0x{:x}",
                    prepared.owner.owner().incarnation(),
                    id,
                    prepared.core.top);
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
pub fn spawn(owner: OwnerRef, loaded: crate::kernel::LoadedImage) -> Result<u64, mac_abi::Status>
{
    let prepared = prepare(owner, loaded).map_err(|failure| failure.status)?;
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
    unsafe {
        core::ptr::write_volatile(0x02004000 as *mut u64,
                                  crate::platform::ticks().wrapping_add(TIMER_INTERVAL));
    }
}
pub fn start() -> !
{
    let mut scheduler = SCHEDULER.lock();
    let mut stack = vec![0u128; STACK_WORDS].into_boxed_slice();
    let top = unsafe { stack.as_mut_ptr().add(stack.len()) } as usize;
    let idle_frame = (top - core::mem::size_of::<TrapFrame>()) as *mut TrapFrame;
    let mut initial = TrapFrame::default();
    initial.pc = idle as *const () as usize;
    initial.status = super::traps::INITIAL_STATUS;
    unsafe {
        idle_frame.write(initial);
    }
    scheduler.idle = Some(Idle { _stack: stack,
                                 frame:  idle_frame as usize });
    let t = scheduler.thread(0)
                     .expect("scheduler bootstrap publishes Finder before starting CPU frames");
    let frame = t.state.get().frame as *mut TrapFrame;
    t.update(|state| state.switches += 1);
    activate_context(Some(&t.core.context));
    CURRENT.store(0, Ordering::Relaxed);
    drop(scheduler);
    arm();
    unsafe {
        asm!("csrs mie, {}",in(reg)128usize,options(nomem,nostack));
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
    let index = CURRENT.load(Ordering::Relaxed);
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
    SCHEDULER.lock()
             .thread(CURRENT.load(Ordering::Relaxed))
             .is_some_and(|thread| thread.state.get().waiting.is_some())
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
    let index = CURRENT.load(Ordering::Relaxed);
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
    if !RECLAIM_PENDING.swap(false, Ordering::AcqRel)
    {
        return;
    }
    let current = CURRENT.load(Ordering::Relaxed);
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
                                            if slot == current
                                            {
                                                RECLAIM_PENDING.store(true, Ordering::Release);
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

pub fn schedule(frame: *mut TrapFrame) -> *mut TrapFrame
{
    // Device input and cursor publication precede all ordinary maintenance.
    // Never enable nested preemption while a resident manager owns its guards.
    let fresh_input = crate::kernel::poll_waiting_events();
    if INPUT_PRIORITY.background_allowed(fresh_input)
    {
        reclaim_retired();
        crate::drivers::pump_jobs();
        crate::toolbox::files::pump_provider_operations();
        crate::toolbox::processes::documents::pump();
        crate::toolbox::processes::retirement::pump();
        crate::toolbox::processes::finder::pump();
    }
    let current = CURRENT.load(Ordering::Relaxed);
    let mut scheduler = SCHEDULER.lock();
    scheduler.ticks += 1;
    if let Some(thread) = scheduler.thread(current)
    {
        thread.update(|state| state.frame = frame as usize);
    }
    else if current == IDLE
    {
        scheduler.idle
                 .as_mut()
                 .expect("scheduler bootstrap installs idle before scheduling")
                 .frame = frame as usize;
    }
    let now = crate::platform::ticks();
    for (_, thread) in scheduler.iter()
    {
        if matches!(thread.state.get().waiting, Some(Wait::Events { deadline, .. }) if now >= deadline)
        {
            thread.update(|state| state.waiting = None);
        }
    }
    let priority = scheduler.iter()
                            .filter(|(_, t)| t.state.get().live)
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
            if state.live && if boosted { state.woken == priority } else { state.waiting.is_none() }
            {
                thread.update(|state| {
                          state.woken = 0;
                          state.switches += 1;
                      });
                let next = state.frame as *mut TrapFrame;
                activate_context(Some(&thread.core.context));
                CURRENT.store(index, Ordering::Relaxed);
                arm();
                return next;
            }
        }
    }
    let next =
        scheduler.idle.as_ref().expect("scheduler bootstrap installs idle before scheduling").frame
        as *mut TrapFrame;
    activate_context(None);
    CURRENT.store(IDLE, Ordering::Relaxed);
    arm();
    next
}

pub fn retired() -> bool
{
    SCHEDULER.lock().thread(CURRENT.load(Ordering::Relaxed)).is_some_and(|t| t.state.get().retired)
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
