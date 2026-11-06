//! CPU primitives the terminal and scheduler paths need: a system-wide barrier,
//! and entry onto a fresh stack without unwinding the caller's.

/// Orders all preceding memory accesses before any that follow.
pub(crate) fn barrier()
{
    // SAFETY: a barrier has no operands and cannot fault.
    unsafe { core::arch::asm!("dsb sy", options(nostack)) };
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
