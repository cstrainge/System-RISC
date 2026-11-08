use std::{
    io,
    mem::zeroed,
    ptr::null_mut,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use libc::{raise, sigaction, sigemptyset, signal, SA_RESTART, SIGINT, SIG_DFL};

static GENERATION: AtomicU64 = AtomicU64::new(0);
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn interrupt(_: i32)
{
    // The handler only touches a lock-free atomic; cleanup runs outside signal context.
    INTERRUPTED.store(true, Ordering::Relaxed);
    GENERATION.fetch_add(1, Ordering::Release);
}

pub fn generation() -> u64
{
    GENERATION.load(Ordering::Acquire)
}

pub fn take_interrupt() -> bool
{
    INTERRUPTED.load(Ordering::Relaxed) && INTERRUPTED.swap(false, Ordering::Relaxed)
}

// Run after script unwinding and diagnostics, preserving the supervisor's signal status.
pub fn terminate_interrupted()
{
    unsafe {
        signal(SIGINT, SIG_DFL);
        raise(SIGINT);
    }
}

pub struct InteractiveInterrupts
{
    previous: sigaction,
}

impl InteractiveInterrupts
{
    pub fn install() -> io::Result<Self>
    {
        // Caught handlers reset to the default on exec, keeping children interruptible.
        // Ignoring SIGINT instead would make executed children ignore it too.
        let mut action: sigaction = unsafe { zeroed() };
        let mut previous: sigaction = unsafe { zeroed() };
        action.sa_sigaction = interrupt as *const () as usize;
        action.sa_flags = SA_RESTART;
        unsafe { sigemptyset(&mut action.sa_mask); }
        if unsafe { sigaction(SIGINT, &action, &mut previous) } == -1
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { previous })
    }
}

impl Drop for InteractiveInterrupts
{
    fn drop(&mut self)
    {
        unsafe { sigaction(SIGINT, &self.previous, null_mut()); }
        take_interrupt();
    }
}
