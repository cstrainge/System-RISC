//! RV64 one-hart timer scheduler. Only this adapter owns saved CPU frames.
//! Timer interrupts preempt application code; synchronous kernel traps run with
//! interrupts masked, so no allocator/manager guard can be preempted by an app.
use alloc::{boxed::Box, vec};
use core::{arch::asm,
           sync::atomic::{AtomicBool, AtomicUsize, Ordering}};

use spinning_top::Spinlock as Mutex;

use super::traps::TrapFrame;
use crate::{context::ThreadContext,
            toolbox::handles::{Kind, Table}};
pub const MAX_THREADS: usize = Kind::Threads.capacity();
const STACK_WORDS: usize = 16384; // 256KiB, including nested service/definition frames.
// The CLINT runs at 10 MHz. At 500 us, two runnable peers can consume the
// entire 1 ms interaction budget before a queued caller resumes. Use 250 us
// for this resident round-robin policy; a freshly woken input consumer still
// takes priority. This is not a hard real-time or future scheduler contract.
const TIMER_INTERVAL: u64 = 2_500;
static CONTEXTS: [ThreadContext; MAX_THREADS] = [const { ThreadContext::new() }; MAX_THREADS];
static CURRENT: AtomicUsize = AtomicUsize::new(MAX_THREADS);
static IO: AtomicBool = AtomicBool::new(false);
static RECLAIM_PENDING: AtomicBool = AtomicBool::new(false);
struct Thread
{
    id:       u32,
    _stack:   Box<[u128]>,
    frame:    usize,
    live:     bool,
    retired:  bool,
    image:    usize,
    switches: u64,
    waiting:  Option<Wait>,
    woken:    u8
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
        job: u32
    }
}
struct Scheduler
{
    threads:    [Option<Thread>; MAX_THREADS],
    identities: Table<usize>,
    idle:       Option<Thread>,
    ticks:      u64
}
static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler { identities: Table::new(Kind::Threads),
                                                            threads:    [const { None };
                                                                         MAX_THREADS],
                                                            idle:       None,
                                                            ticks:      0 });
unsafe extern "C" {
    fn __system_start_frame(frame: *mut TrapFrame) -> !;
}
pub fn current_context() -> &'static ThreadContext
{
    if IO.load(Ordering::Relaxed)
    {
        return crate::context::boot_context();
    }
    CONTEXTS.get(CURRENT.load(Ordering::Relaxed)).unwrap_or(crate::context::boot_context())
}
pub fn image() -> usize
{
    let index = CURRENT.load(Ordering::Relaxed);
    SCHEDULER.lock().threads.get(index).and_then(Option::as_ref).map_or(0, |t| t.image)
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
    let s = SCHEDULER.lock();
    s.threads
     .iter()
     .enumerate()
     .find_map(|(i, t)| {
         t.as_ref().filter(|t| t.live && CONTEXTS[i].identity().app.0 == app).map(|t| t.switches)
     })
     .unwrap_or(0)
}

/// Retired records count too: their stacks and invocation contexts have not
/// yet been reclaimed by another thread. No scheduler guard escapes this read.
pub(crate) fn has_threads(app: u64) -> bool
{
    SCHEDULER.lock()
             .threads
             .iter()
             .enumerate()
             .any(|(index, thread)| thread.is_some() && CONTEXTS[index].identity().app.0 == app)
}
pub fn spawn(owner: crate::toolbox::handles::OwnerRef, image: usize, entry: usize)
             -> Result<u64, mac_abi::Status>
{
    let mut scheduler = SCHEDULER.lock();
    let slot = scheduler.threads.iter().position(|t| t.is_none()).ok_or(mac_abi::NO_MEMORY)?;
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
    let top = unsafe { stack.as_mut_ptr().add(stack.len()) } as usize;
    let frame = (top - core::mem::size_of::<TrapFrame>()) as *mut TrapFrame;
    let mut initial = TrapFrame::default();
    initial.pc = app_start as *const () as usize;
    initial.status = super::traps::INITIAL_STATUS;
    initial.registers[8] = entry;
    // gp is not initialized by our freestanding executables; rustc uses PC-relative addressing.
    unsafe {
        frame.write(initial);
    }
    let id = scheduler.identities.insert(slot).map_err(|_| mac_abi::NO_MEMORY)?;
    let thread = u64::from(id);
    let app = owner.owner().incarnation();
    if let Err(error) = CONTEXTS[slot].assign(owner, thread)
    {
        scheduler.identities.remove(id);
        return Err(error);
    }
    scheduler.threads[slot] = Some(Thread { id,
                                            _stack: stack,
                                            frame: frame as usize,
                                            live: true,
                                            retired: false,
                                            image,
                                            switches: 0,
                                            waiting: None,
                                            woken: 0 });
    serial!("PROCESS_THREAD app={} thread={} stack=0x{:x}", app, thread, top);
    Ok(thread)
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
    scheduler.idle = Some(Thread { id:       0,
                                   _stack:   stack,
                                   frame:    idle_frame as usize,
                                   live:     true,
                                   retired:  false,
                                   image:    0,
                                   switches: 0,
                                   waiting:  None,
                                   woken:    0 });
    let t = scheduler.threads[0].as_mut().expect("Finder thread");
    let frame = t.frame as *mut TrapFrame;
    t.switches += 1;
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
    if let Some(thread) = SCHEDULER.lock().threads.get_mut(index).and_then(Option::as_mut)
    {
        thread.waiting = Some(Wait::Events { mask,
                                             deadline });
        thread.woken = 0;
    }
}

pub fn blocked() -> bool
{
    let index = CURRENT.load(Ordering::Relaxed);
    SCHEDULER.lock()
             .threads
             .get(index)
             .and_then(Option::as_ref)
             .is_some_and(|thread| thread.waiting.is_some())
}

/// Copy identities and masks before inspecting the resident Event Manager. No
/// scheduler lock may remain held while a service enters application resources.
pub fn waiting() -> [(u64, u16); MAX_THREADS]
{
    let scheduler = SCHEDULER.lock();
    core::array::from_fn(|index| {
        scheduler.threads[index].as_ref()
                                .filter(|thread| thread.live)
                                .and_then(|thread| {
                                    match thread.waiting
                                    {
                                        Some(Wait::Events { mask,
                                                            .. }) =>
                                        {
                                            Some((CONTEXTS[index].identity().app.0, mask))
                                        }
                                        _ => None
                                    }
                                })
                                .unwrap_or((0, 0))
    })
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
    let mut scheduler = SCHEDULER.lock();
    for (index, thread) in scheduler.threads.iter_mut().enumerate()
    {
        if CONTEXTS[index].identity().app.0 == app
           && let Some(thread) = thread.as_mut().filter(|thread| thread.live)
           && matches!(thread.waiting, Some(Wait::Events { .. }))
        {
            thread.waiting = None;
            thread.woken = priority;
            break;
        }
    }
}
/// Park only after the original trap has dropped every manager guard. The
/// replay keeps the original PC/registers, and borrows fresh buffers on wake.
/// Checking completion while holding the scheduler guard closes completion-
/// before-park races; the job pump releases its queue guard before waking us.
pub fn wait_io(job: u32) -> Result<bool, mac_abi::Status>
{
    let index = CURRENT.load(Ordering::Relaxed);
    let mut scheduler = SCHEDULER.lock();
    let thread = scheduler.threads
                          .get_mut(index)
                          .and_then(Option::as_mut)
                          .filter(|thread| thread.live)
                          .ok_or(mac_abi::WRONG_THREAD)?;
    let pending = if crate::toolbox::handles::Kind::FileTransfers.contains_wire_id(job)
    {
        crate::toolbox::files::transfer_io_pending(u64::from(thread.id), job)?
    }
    else
    {
        crate::drivers::job_pending(u64::from(thread.id), job)?
    };
    if !pending
    {
        return Ok(false);
    }
    thread.waiting = Some(Wait::Io { job });
    thread.woken = 0;
    Ok(true)
}

pub fn wake_io(identity: u64, job: u32)
{
    let Ok(id) = u32::try_from(identity)
    else
    {
        return;
    };
    let mut scheduler = SCHEDULER.lock();
    let Some(&index) = scheduler.identities.get(id)
    else
    {
        return;
    };
    if let Some(thread) =
        scheduler.threads[index].as_mut().filter(|thread| thread.id == id && thread.live)
       && matches!(thread.waiting, Some(Wait::Io { job: expected }) if expected == job)
    {
        thread.waiting = None;
        thread.woken = 1;
    }
}

pub fn retire_current()
{
    let index = CURRENT.load(Ordering::Relaxed);
    let thread = {
        let mut scheduler = SCHEDULER.lock();
        if let Some(thread) = scheduler.threads.get_mut(index).and_then(Option::as_mut)
        {
            thread.live = false;
            thread.retired = true;
            Some(thread.id)
        }
        else
        {
            None
        }
    };
    if let Some(thread) = thread
    {
        RECLAIM_PENDING.store(true, Ordering::Release);
        crate::drivers::discard_thread_jobs(u64::from(thread));
    }
}

/// Safe only at a resident, interrupt-masked boundary. The calling stack is
/// identified by the actual scheduler context; it is never reclaimed here.
pub(crate) fn reclaim_retired()
{
    if !RECLAIM_PENDING.load(Ordering::Acquire)
    {
        return;
    }
    let current = CURRENT.load(Ordering::Relaxed);
    let mut reclaimed = false;
    if RECLAIM_PENDING.swap(false, Ordering::AcqRel)
    {
        let mut scheduler = SCHEDULER.lock();
        // The CPU is executing this trap on `current`'s stack. Only another
        // stack's retired frame/context may be destroyed here.
        for index in 0..MAX_THREADS
        {
            if index == current
               && scheduler.threads[index].as_ref().is_some_and(|thread| thread.retired)
            {
                RECLAIM_PENDING.store(true, Ordering::Release);
                continue;
            }
            if index != current
               && scheduler.threads[index].as_ref().is_some_and(|thread| thread.retired)
               && let Some(thread) = scheduler.threads[index].take()
            {
                let app = CONTEXTS[index].identity().app.0;
                scheduler.identities.remove(thread.id);
                CONTEXTS[index].release();
                let id = thread.id;
                drop(thread);
                reclaimed = true;
                serial!("PROCESS_THREAD_RECLAIMED app={app} thread={id}");
            }
        }
    }
    if reclaimed
    {
        crate::toolbox::processes::retirement::thread_reclaimed();
    }
}

pub fn schedule(frame: *mut TrapFrame) -> *mut TrapFrame
{
    reclaim_retired();
    crate::drivers::pump_jobs();
    crate::toolbox::files::pump_provider_operations();
    crate::toolbox::processes::documents::pump();
    crate::toolbox::processes::retirement::pump();
    crate::kernel::poll_waiting_events();
    let current = CURRENT.load(Ordering::Relaxed);
    let mut scheduler = SCHEDULER.lock();
    scheduler.ticks += 1;
    if let Some(thread) = scheduler.threads.get_mut(current).and_then(Option::as_mut)
    {
        thread.frame = frame as usize;
    }
    else if current == MAX_THREADS
    {
        scheduler.idle.as_mut().expect("idle thread").frame = frame as usize;
    }
    let now = crate::platform::ticks();
    for thread in scheduler.threads.iter_mut().flatten()
    {
        if matches!(thread.waiting, Some(Wait::Events { deadline, .. }) if now >= deadline)
        {
            thread.waiting = None;
        }
    }
    // An event consumer must not wait behind every CPU-bound application. Only
    // real event readiness gets this boost; periodic idle timeouts use normal RR.
    let priority = scheduler.threads
                            .iter()
                            .flatten()
                            .filter(|thread| thread.live)
                            .map(|thread| thread.woken)
                            .max()
                            .unwrap_or(0);
    if priority != 0
    {
        for offset in 1..=MAX_THREADS
        {
            let index = (current + offset) % MAX_THREADS;
            if let Some(thread) =
                scheduler.threads[index].as_mut()
                                        .filter(|thread| thread.live && thread.woken == priority)
            {
                thread.woken = 0;
                thread.switches += 1;
                let next = thread.frame as *mut TrapFrame;
                CURRENT.store(index, Ordering::Relaxed);
                arm();
                return next;
            }
        }
    }
    for offset in 1..=MAX_THREADS
    {
        let index = (current + offset) % MAX_THREADS;
        if let Some(thread) =
            scheduler.threads[index].as_mut().filter(|t| t.live && t.waiting.is_none())
        {
            thread.switches += 1;
            let next = thread.frame as *mut TrapFrame;
            CURRENT.store(index, Ordering::Relaxed);
            arm();
            return next;
        }
    }
    let next = scheduler.idle.as_ref().expect("idle thread").frame as *mut TrapFrame;
    CURRENT.store(MAX_THREADS, Ordering::Relaxed);
    arm();
    next
}

pub fn retired() -> bool
{
    let index = CURRENT.load(Ordering::Relaxed);
    SCHEDULER.lock().threads.get(index).and_then(Option::as_ref).is_some_and(|t| t.retired)
}

pub fn image_for(app: u64) -> Option<usize>
{
    let s = SCHEDULER.lock();
    s.threads.iter().enumerate().find_map(|(index, thread)| {
                                    thread.as_ref()
                                          .filter(|_| CONTEXTS[index].identity().app.0 == app)
                                          .map(|t| t.image)
                                })
}
