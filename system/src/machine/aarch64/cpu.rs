//! CPU primitives the terminal and scheduler paths need: a system-wide barrier,
//! and entry onto a fresh stack without unwinding the caller's.

/// Orders all preceding memory accesses before any that follow.
pub(crate) fn barrier()
{
    // SAFETY: a barrier has no operands and cannot fault.
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
}

/// Programs the next timer deadline, in ticks of the platform's time counter.
///
/// The generic timer's compare value is written before its interrupt is unmasked,
/// so a stale compare cannot fire while the deadline is being changed.
pub(crate) fn set_timer_deadline(deadline: u64)
{
    let frequency: u64;
    // SAFETY: reading the counter frequency has no side effects.
    unsafe { core::arch::asm!("mrs {frequency}, cntfrq_el0", frequency = out(reg) frequency) };
    let _ = frequency;
    // SAFETY: writes this CPU's timer compare and unmasks only the timer
    // interrupt; both are per-CPU state this path owns.
    unsafe {
        core::arch::asm!("msr cntp_cval_el0, {deadline}", "msr cntp_ctl_el0, {enable}",
            deadline = in(reg) deadline, enable = in(reg) 1u64);
    }
}

/// Masks the timer interrupt, leaving other interrupt sources untouched.
pub(crate) fn disable_timer_interrupt()
{
    // SAFETY: clears only this CPU's timer enable bit.
    unsafe { core::arch::asm!("msr cntp_ctl_el0, {disable}", disable = in(reg) 0u64) };
}

/// Waits for the next interrupt. This is quiescence, not a spin: the CPU stops
/// until an event arrives.
pub(crate) fn wait_for_interrupt()
{
    // SAFETY: `wfi` is a hint that cannot fault and resumes on any interrupt.
    unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
}

core::arch::global_asm!(".section .text", ".balign 4", ".global __system_terminal_stack",
                        "__system_terminal_stack:", "mov sp, x0", "and sp, sp, #-16", "mov x9, \
                        x1", "mov x0, x2", "mov x30, xzr", "br x9");

unsafe extern "C" {
    fn __system_terminal_stack(top: usize, entry: unsafe extern "C" fn(*const ()) -> !,
                               argument: *const ())
                               -> !;
}

/// Abandons the current stack and enters `entry` on `top`.
///
/// The terminal path cannot return to an interrupted ordinary service, so it
/// must not leave a resume point behind: the link register is cleared and the
/// caller's stack is dropped rather than unwound.
///
/// # Safety
/// `top` must be the top of a mapped, writable stack that nothing else is using,
/// and `entry` must never return.
pub(crate) unsafe fn terminal_stack(top: usize, entry: unsafe extern "C" fn(*const ()) -> !,
                                    argument: *const ())
                                    -> !
{
    // SAFETY: the caller guarantees the stack and the entry contract.
    unsafe { __system_terminal_stack(top, entry, argument) }
}
