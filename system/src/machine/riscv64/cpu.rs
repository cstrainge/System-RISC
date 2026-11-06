//! CPU primitives the terminal and scheduler paths need: a system-wide barrier,
//! and entry onto a fresh stack without unwinding the caller's.

/// Orders all preceding memory accesses before any that follow.
pub(crate) fn barrier()
{
    // SAFETY: a fence has no operands and cannot fault.
    unsafe { core::arch::asm!("fence iorw, iorw", options(nostack)) };
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
