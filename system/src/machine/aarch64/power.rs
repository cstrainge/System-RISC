//! Terminal-only board failure and restart, independent of ordinary power
//! drivers.
//!
//! The address and the values written come from the checked platform record, so
//! this file holds only the instruction-level differences: masking IRQ in
//! PSTATE.DAIF, a system-wide barrier, and waiting for the controller to finish.
//! No callbacks, ROM logger, scheduler context, heap or locks are consulted.
//! These terminal paths abandon execution; they do not claim privileged-fault
//! recovery.
pub(crate) mod platform
{
    include!(concat!(env!("OUT_DIR"), "/platform.rs"));
}

pub(crate) fn fatal_shutdown() -> !
{
    // Terminal execution cannot return to an interrupted ordinary service.
    // SAFETY: masking IRQ and waiting changes only this CPU's state.
    unsafe { core::arch::asm!("msr daifset, #2", options(nostack)) };
    crate::machine::emergency::serial(format_args!("SYSTEM_SHUTDOWN: fatal privileged fault; recovery is \
                                           not proven\n"));
    // A zero address is the record's absence marker: this machine declares no
    // power-management device, so there is no register that completes a
    // shutdown. Report that plainly and halt rather than writing to address
    // zero, which would fault inside the terminal path itself.
    if platform::FATAL_FINISHER_ADDRESS != 0
    {
        // SAFETY: the current platform declares this finisher window in its
        // record. The write requests the board's guest-panic shutdown and
        // execution never resumes; no heap, driver registry or borrowed desktop
        // service is involved.
        unsafe {
            core::ptr::write_volatile(platform::FATAL_FINISHER_ADDRESS as *mut u32,
                                      platform::FATAL_FINISHER_FAILURE);
            core::arch::asm!("dsb sy", options(nostack));
        }
    }
    else
    {
        crate::machine::emergency::serial(format_args!("SYSTEM_SHUTDOWN_NO_FINISHER: this machine \
                                                        declares no power-management device; halting \
                                                        instead of powering off\n"));
    }
    // The controller completes the shutdown asynchronously. This is quiescence
    // after an explicit, reported shutdown request, never a silent spin.
    loop
    {
        unsafe { core::arch::asm!("wfi", options(nostack)) };
    }
}

/// Requests a platform reset only after the emergency terminal has abandoned all
/// ordinary execution. Graceful application restart and shutdown continue
/// through the normal driver-selected System power service.
pub(crate) fn emergency_restart() -> !
{
    // SAFETY: as `fatal_shutdown`; only this CPU's mask state changes.
    unsafe { core::arch::asm!("msr daifset, #2", options(nostack)) };
    crate::machine::emergency::serial(format_args!("SYSTEM_RESTART: emergency terminal requested reset\n"));
    // As `fatal_shutdown`: no finisher register exists on this machine, so the
    // request is reported and the CPU halts rather than a reset being claimed.
    if platform::FATAL_FINISHER_ADDRESS != 0
    {
        // SAFETY: terminal admission retains the mapped checked board register
        // and execution never resumes. The record supplies the restart value.
        unsafe {
            core::ptr::write_volatile(platform::FATAL_FINISHER_ADDRESS as *mut u32,
                                      platform::FATAL_FINISHER_RESTART);
            core::arch::asm!("dsb sy", options(nostack));
        }
    }
    else
    {
        crate::machine::emergency::serial(format_args!("SYSTEM_RESTART_NO_FINISHER: this machine \
                                                        declares no power-management device; halting \
                                                        instead of resetting\n"));
    }
    loop
    {
        unsafe { core::arch::asm!("wfi", options(nostack)) };
    }
}
