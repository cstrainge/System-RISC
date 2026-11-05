//! What a shutdown or restart request becomes when the machine has no
//! power-management device.
//!
//! A machine either has a register that powers it off or it does not. When it
//! does, the architecture's own code writes it, because the register form and
//! the barrier are that architecture's. When it does not, the answer is the same
//! on every machine, so it is stated here once rather than repeated per
//! architecture: abandon ordinary execution, stop the other CPUs, and show the
//! classic "safe to turn off" notice with a Restart button.
//!
//! This is the standard path, not a fallback for one board. Any platform whose
//! record declares no finisher reaches it, and a platform that later gains one
//! simply stops reaching it.

/// Which request could not be satisfied.
#[derive(Clone, Copy)]
pub(crate) enum Request
{
    Shutdown,
    Restart
}

/// Reports the missing finisher and halts, for when there is no display to say
/// it on. `request` selects the wording; the machine cannot do either.
pub(crate) fn halt_with_report(request: Request) -> !
{
    match request
    {
        Request::Shutdown =>
        {
            crate::machine::emergency::serial(format_args!("SYSTEM_SHUTDOWN_NO_FINISHER: this \
                                                            machine declares no power-management \
                                                            device and no terminal display is \
                                                            available; halting\n"))
        }
        Request::Restart =>
        {
            crate::machine::emergency::serial(format_args!("SYSTEM_RESTART_NO_FINISHER: this \
                                                            machine declares no power-management \
                                                            device and no terminal display is \
                                                            available; halting\n"))
        }
    }
    crate::machine::power::halt()
}

/// The classic notice, or a reported halt when the display is unavailable.
///
/// `try_safe_to_turn_off` returns only when the terminal path is already
/// running, where presenting the screen again would recurse into the stop path.
pub(crate) fn without_finisher(request: Request) -> !
{
    if crate::machine::emergency::try_safe_to_turn_off()
    {
        unreachable!("the safe-to-turn-off screen does not return");
    }
    halt_with_report(request)
}
