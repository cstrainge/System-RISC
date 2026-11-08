
use std::{ io, mem::zeroed, ptr::null_mut };

use libc::{ getpgrp, isatty, kill, pid_t, pthread_sigmask, sigaddset, sigemptyset, sigset_t,
            tcgetpgrp, tcsetpgrp, SIG_BLOCK, SIG_SETMASK, SIGCONT, SIGTTOU, STDIN_FILENO };


// Only a pipeline source inherits terminal input. Its private process group must
// own that terminal while reading, then return ownership even during unwinding.
pub(crate) struct Foreground
{
    previous: pid_t,
}


fn select(group: pid_t) -> io::Result<()>
{
    // Restoring the editor means calling from a background group. Block SIGTTOU
    // only on this thread during tcsetpgrp, preserving the caller's signal mask.
    let mut blocked: sigset_t = unsafe { zeroed() };
    let mut previous: sigset_t = unsafe { zeroed() };
    unsafe { sigemptyset(&mut blocked); sigaddset(&mut blocked, SIGTTOU); }
    let result = unsafe { pthread_sigmask(SIG_BLOCK, &blocked, &mut previous) };
    if result != 0 { return Err(io::Error::from_raw_os_error(result)); }
    let result = if unsafe { tcsetpgrp(STDIN_FILENO, group) } == -1
        { Err(io::Error::last_os_error()) } else { Ok(()) };
    unsafe { pthread_sigmask(SIG_SETMASK, &previous, null_mut()); }
    result
}


impl Foreground
{
    pub(crate) fn acquire(group: pid_t) -> io::Result<Option<Self>>
    {
        if unsafe { isatty(STDIN_FILENO) } != 1 { return Ok(None); }
        let previous = unsafe { tcgetpgrp(STDIN_FILENO) };
        if previous == -1 { return Err(io::Error::last_os_error()); }
        if previous != unsafe { getpgrp() }
        {
            return Err(io::Error::new(io::ErrorKind::WouldBlock,
                "Another process group already owns terminal input"));
        }
        select(group)?;
        // A fast child can attempt its first read before the handoff and stop on
        // SIGTTIN. Resume the owned group after it becomes foreground.
        unsafe { kill(-group, SIGCONT); }
        Ok(Some(Self { previous }))
    }
}


impl Drop for Foreground
{
    fn drop(&mut self) { let _ = select(self.previous); }
}
