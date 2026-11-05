//! CPU primitives the terminal and scheduler paths need: a system-wide barrier,
//! and entry onto a fresh stack without unwinding the caller's.

/// Orders all preceding memory accesses before any that follow.
pub(crate) fn barrier()
{
    // SAFETY: a fence has no operands and cannot fault.
    unsafe { core::arch::asm!("fence iorw, iorw", options(nostack)) };
}

/// Programs the next timer deadline, in ticks of the platform's time counter.
///
/// The supervisor timer was admitted by firmware before supervisor entry. The
/// timer interrupt is enabled only when an actual deadline is set, and only when
/// hardware reports it clear, so a separate always-on interrupt source is
/// preserved rather than overwritten.
pub(crate) fn set_timer_deadline(deadline: u64)
{
    const TIMER_INTERRUPT: usize = 32;
    // SAFETY: STIMECMP and sie are supervisor CSRs this path owns; the sequence
    // writes one deadline and enables one interrupt bit.
    unsafe {
        core::arch::asm!("csrw 0x14d, {deadline}",
            "csrr {enabled}, sie",
            "and {enabled}, {enabled}, {timer}",
            "bnez {enabled}, 2f",
            "csrs sie, {timer}",
            "2:",
            deadline = in(reg) deadline,
            timer = in(reg) TIMER_INTERRUPT,
            enabled = out(reg) _,
            options(nostack));
    }
}

/// Masks the timer interrupt, leaving other interrupt sources untouched.
pub(crate) fn disable_timer_interrupt()
{
    const TIMER_INTERRUPT: usize = 32;
    // SAFETY: clears one bit of this hart's interrupt-enable CSR.
    unsafe {
        core::arch::asm!("csrc sie, {timer}", timer = in(reg) TIMER_INTERRUPT, options(nostack));
    }
}

/// Waits for the next interrupt. This is quiescence, not a spin: the CPU stops
/// until an event arrives.
pub(crate) fn wait_for_interrupt()
{
    // SAFETY: `wfi` is a hint that cannot fault and resumes on any interrupt.
    unsafe { core::arch::asm!("wfi", options(nomem, nostack)) };
}

core::arch::global_asm!(".section .text", ".balign 4", ".global __system_terminal_stack",
                        "__system_terminal_stack:", "mv sp, a0", "andi sp, sp, -16", "mv t0, a1",
                        "mv a0, a2", "li ra, 0", "jr t0");

unsafe extern "C" {
    fn __system_terminal_stack(top: usize, entry: unsafe extern "C" fn(*const ()) -> !,
                               argument: *const ())
                               -> !;
}

/// Abandons the current stack and enters `entry` on `top`.
///
/// The terminal path cannot return to an interrupted ordinary service, so it
/// must not leave a resume point behind: the return address is cleared and the
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
