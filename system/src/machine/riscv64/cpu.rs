//! CPU primitives the terminal and scheduler paths need: a system-wide barrier,
//! and entry onto a fresh stack without unwinding the caller's.
use core::ptr::NonNull;

use crate::machine::traps::hart::Control;

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

/// Monotonic time counter, in platform ticks.
pub(crate) fn ticks() -> u64
{
    let value: u64;
    // SAFETY: reading the time counter has no side effects. The fences stop the
    // compiler and the hardware reordering the read across surrounding stores.
    unsafe {
        core::arch::asm!("fence rw, i", "rdtime {value}", "fence i, rw", value = out(reg) value);
    }
    value
}

/// The timer compare value currently programmed.
pub(crate) fn timer_compare() -> u64
{
    let value: u64;
    // SAFETY: reading a supervisor CSR has no side effects.
    unsafe { core::arch::asm!("csrr {}, stimecmp", out(reg) value, options(nomem, nostack)) };
    value
}

/// Installs the trap vector: the address the CPU jumps to on any trap.
///
/// # Safety
/// `vector` must be the entry of a valid, mapped trap handler that preserves the
/// frame layout this kernel expects.
pub(crate) unsafe fn install_trap_vector(vector: usize)
{
    // SAFETY: the caller guarantees the handler contract.
    unsafe { core::arch::asm!("csrw stvec, {}", in(reg) vector) };
}

/// Bytes per vector register, or zero when the extension is absent.
pub(crate) fn vector_bytes() -> usize
{
    let bytes: usize;
    // SAFETY: reading vlenb is defined even where the V extension is absent,
    // where it reads as zero.
    unsafe { core::arch::asm!("csrr {}, vlenb", out(reg) bytes, options(nomem, nostack)) };
    bytes
}

/// The current stack pointer.
pub(crate) fn stack_pointer() -> usize
{
    let value: usize;
    // SAFETY: reading sp has no side effects.
    unsafe { core::arch::asm!("mv {}, sp", out(reg) value, options(nomem, nostack)) };
    value
}

/// This hart's control-block pointer, kept in the supervisor scratch register.
pub(crate) fn hart_control() -> usize
{
    let value: usize;
    // SAFETY: reading sscratch has no side effects.
    unsafe { core::arch::asm!("csrr {}, sscratch", out(reg) value, options(nomem, nostack)) };
    value
}

/// Sets this hart's control-block pointer.
///
/// # Safety
/// `control` must point to the live control block for this hart. Non-nullness is
/// in the type, not in this note: every caller publishes a real control block,
/// and a hart that named none would only fail on its next trap.
pub(crate) unsafe fn set_hart_control(control: NonNull<Control>)
{
    // SAFETY: the caller guarantees the pointer; a `NonNull` is never null.
    unsafe {
        core::arch::asm!("csrw sscratch, {}", in(reg) control.as_ptr(), options(nostack));
    }
}

/// Raises a breakpoint, entering the trap handler with a breakpoint cause.
pub(crate) fn breakpoint() -> !
{
    // SAFETY: `ebreak` transfers control to the installed trap vector and never
    // returns to the next instruction.
    unsafe { core::arch::asm!("ebreak", options(noreturn)) };
}

/// Clears a pending software interrupt so re-entry cannot immediately retrap.
pub(crate) fn clear_software_interrupt()
{
    // SAFETY: clears one pending bit for this hart.
    unsafe { core::arch::asm!("csrci sip, 2", options(nostack)) };
}

/// Unmasks the software-interrupt source, which the scheduler uses to admit a
/// peer into idle.
pub(crate) fn enable_software_interrupt()
{
    // SAFETY: sets one interrupt-enable bit for this hart.
    unsafe { core::arch::asm!("csrs sie, {}", in(reg) 2usize, options(nomem, nostack)) };
}

/// The interrupt-enable register, whose bits say which sources can interrupt.
pub(crate) fn interrupt_enables() -> usize
{
    let value: usize;
    // SAFETY: reading sie has no side effects.
    unsafe { core::arch::asm!("csrr {}, sie", out(reg) value, options(nomem, nostack)) };
    value
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

/// Orders instruction-cache maintenance against later fetches. Written code is
/// not guaranteed to be visible to the fetch pipeline without this.
pub(crate) fn instruction_barrier()
{
    // SAFETY: the fences have no operands and cannot fault.
    unsafe { core::arch::asm!("fence rw, rw", "fence.i", options(nostack)) };
}