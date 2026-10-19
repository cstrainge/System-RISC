//! RV64 one-hart timer scheduler. Only this adapter owns saved CPU frames.
//! Timer interrupts preempt application code; synchronous kernel traps run with
//! interrupts masked, so no allocator/manager guard can be preempted by an app.
use alloc::{boxed::Box, vec};
use core::{arch::asm,
           sync::atomic::{AtomicBool, AtomicUsize, Ordering}};

use spinning_top::Spinlock as Mutex;

use super::traps::TrapFrame;
use crate::context::ThreadContext;
pub const MAX_THREADS: usize = 8;
const STACK_WORDS: usize = 16384; // 256KiB, including nested service/definition frames.
static CONTEXTS: [ThreadContext; MAX_THREADS] = [const { ThreadContext::new() }; MAX_THREADS];
static CURRENT: AtomicUsize = AtomicUsize::new(MAX_THREADS);
static IO: AtomicBool = AtomicBool::new(false);
struct Thread
{
    _stack:   Box<[u128]>,
    frame:    usize,
    live:     bool,
    retired:  bool,
    image:    usize,
    switches: u64
}
struct Scheduler
{
    threads: [Option<Thread>; MAX_THREADS],
    ticks:   u64
}
static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler { threads: [const { None }; MAX_THREADS],
                                                            ticks:   0 });
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
pub fn spawn(app: u64, image: usize, entry: usize) -> Result<u64, mac_abi::Status>
{
    let mut scheduler = SCHEDULER.lock();
    let slot = scheduler.threads.iter().position(|t| t.is_none()).ok_or(mac_abi::NO_MEMORY)?;
    let mut stack = vec![0u128; STACK_WORDS].into_boxed_slice();
    let top = unsafe { stack.as_mut_ptr().add(stack.len()) } as usize;
    let frame = (top - core::mem::size_of::<TrapFrame>()) as *mut TrapFrame;
    let mut initial = TrapFrame::default();
    initial.pc = app_start as *const () as usize;
    initial.status = 0x1880;
    initial.registers[8] = entry;
    // gp is not initialized by our freestanding executables; rustc uses PC-relative addressing.
    unsafe {
        frame.write(initial);
    }
    let thread = app.checked_add(1).ok_or(mac_abi::NO_MEMORY)?;
    CONTEXTS[slot].assign(app, thread);
    scheduler.threads[slot] = Some(Thread { _stack: stack,
                                            frame: frame as usize,
                                            live: true,
                                            retired: false,
                                            image,
                                            switches: 0 });
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
                                  crate::platform::ticks().wrapping_add(100_000));
    }
}
pub fn start() -> !
{
    let mut scheduler = SCHEDULER.lock();
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
pub fn retire_current()
{
    let index = CURRENT.load(Ordering::Relaxed);
    if let Some(t) = SCHEDULER.lock().threads.get_mut(index).and_then(Option::as_mut)
    {
        t.live = false;
        t.retired = true;
    }
}
pub fn schedule(frame: *mut TrapFrame) -> *mut TrapFrame
{
    arm();
    let current = CURRENT.load(Ordering::Relaxed);
    let mut scheduler = SCHEDULER.lock();
    scheduler.ticks += 1;
    // A retired stack is reclaimed only after a different stack has become current.
    for (i, thread) in scheduler.threads.iter_mut().enumerate()
    {
        if i != current && thread.as_ref().is_some_and(|t| t.retired)
        {
            *thread = None;
        }
    }
    if let Some(thread) = scheduler.threads.get_mut(current).and_then(Option::as_mut)
    {
        thread.frame = frame as usize;
    }
    for offset in 1..=MAX_THREADS
    {
        let index = (current + offset) % MAX_THREADS;
        if let Some(thread) = scheduler.threads[index].as_mut().filter(|t| t.live)
        {
            thread.switches += 1;
            let next = thread.frame as *mut TrapFrame;
            CURRENT.store(index, Ordering::Relaxed);
            return next;
        }
    }
    // Finder is protected from quitting; it is always a runnable fallback.
    frame
}

pub fn slot() -> usize
{
    CURRENT.load(Ordering::Relaxed).min(MAX_THREADS - 1)
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
                                          .filter(|t| {
                                              t.live && CONTEXTS[index].identity().app.0 == app
                                          })
                                          .map(|t| t.image)
                                })
}
