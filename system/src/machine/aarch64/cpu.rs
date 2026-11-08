//! CPU primitives the terminal and scheduler paths need: a system-wide barrier,
//! and entry onto a fresh stack without unwinding the caller's.
use core::ptr::NonNull;

use crate::machine::traps::hart::Control;

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

/// Monotonic time counter, in platform ticks of the generic timer.
pub(crate) fn ticks() -> u64
{
    let value: u64;
    // SAFETY: reading the virtual counter has no side effects.
    unsafe { core::arch::asm!("mrs {}, cntvct_el0", out(reg) value, options(nomem, nostack)) };
    value
}

/// The timer compare value currently programmed.
pub(crate) fn timer_compare() -> u64
{
    let value: u64;
    // SAFETY: reading this CPU's timer compare has no side effects.
    unsafe { core::arch::asm!("mrs {}, cntp_cval_el0", out(reg) value, options(nomem, nostack)) };
    value
}

/// Installs the trap vector: the address the CPU jumps to on any exception.
///
/// # Safety
/// `vector` must be the entry of a valid, mapped exception table that preserves
/// the frame layout this kernel expects.
pub(crate) unsafe fn install_trap_vector(vector: usize)
{
    // SAFETY: the caller guarantees the handler contract. VBAR_EL1 must be
    // 2 KiB aligned, which the table's own alignment attribute provides.
    unsafe {
        core::arch::asm!("msr vbar_el1, {}", "isb", in(reg) vector);
    }
}

/// Bytes per vector register. AArch64 has no such extension, so this reports
/// zero and callers size their frames accordingly.
pub(crate) fn vector_bytes() -> usize
{
    0
}

/// The current stack pointer.
pub(crate) fn stack_pointer() -> usize
{
    let value: usize;
    // SAFETY: reading sp has no side effects.
    unsafe { core::arch::asm!("mov {}, sp", out(reg) value, options(nomem, nostack)) };
    value
}

/// This CPU's control-block pointer, kept in TPIDR_EL1.
pub(crate) fn hart_control() -> usize
{
    let value: usize;
    // SAFETY: reading TPIDR_EL1 has no side effects.
    unsafe { core::arch::asm!("mrs {}, tpidr_el1", out(reg) value, options(nomem, nostack)) };
    value
}

/// Sets this CPU's control-block pointer.
///
/// # Safety
/// `control` must point to the live control block for this CPU. Non-nullness is
/// in the type, not in this note: every caller publishes a real control block,
/// and a hart that named none would only fail on its next trap.
pub(crate) unsafe fn set_hart_control(control: NonNull<Control>)
{
    // SAFETY: the caller guarantees the pointer; a `NonNull` is never null.
    unsafe {
        core::arch::asm!("msr tpidr_el1, {}", in(reg) control.as_ptr(), options(nostack));
    }
}

/// Raises a breakpoint, entering the exception handler with a breakpoint cause.
pub(crate) fn breakpoint() -> !
{
    // SAFETY: `brk` transfers control to the installed vector and never returns
    // to the next instruction.
    unsafe { core::arch::asm!("brk #0", options(noreturn)) };
}

/// Makes the source another hart would wake this one through deliverable.
///
/// This machine signals between harts through the interrupt controller, not
/// through a per-hart pending bit, so the adapter that owns the controller
/// enables the software-generated bank. The resident scheduler is single-hart
/// and posts none of them; a hart that has been told it may be woken still has
/// to have the source enabled, or the wake it waits for cannot arrive.
pub(crate) fn admit_wake_source()
{
    super::gic::admit_wake_source();
}

/// The interrupt-enable register, whose bits say which sources can interrupt.
/// AArch64 masks at the controller, so the CPU-level answer is whether IRQ is
/// currently unmasked.
pub(crate) fn interrupt_enables() -> usize
{
    usize::from(super::interrupts::enabled())
}

core::arch::global_asm!(".section .text", ".balign 4", ".global __system_terminal_stack",
                        "__system_terminal_stack:", "mov x9, #15", "bic x9, x0, x9", "mov sp, \
                        x9", "mov x9, x1", "mov x0, x2", "mov x30, xzr", "br x9");

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

/// Orders instruction-cache maintenance against later fetches. Written code is
/// not guaranteed to be visible to the fetch pipeline without this.
pub(crate) fn instruction_barrier()
{
    // SAFETY: the barriers have no operands and cannot fault.
    unsafe { core::arch::asm!("dsb ish", "isb", options(nostack)) };
}
/// This CPU's hardware identity, from MPIDR_EL1. Affinity level 0 is the core
/// within a cluster; the full value identifies the core across clusters.
pub(crate) fn hart_id() -> usize
{
    let value: u64;
    // SAFETY: reading MPIDR_EL1 has no side effects.
    unsafe { core::arch::asm!("mrs {}, mpidr_el1", out(reg) value, options(nomem, nostack)) };
    (value & 0xFF) as usize
}