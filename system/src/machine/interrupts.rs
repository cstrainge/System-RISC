//! Local interrupt/preemption gate for synchronous resident metadata operations.
//! CSR encoding belongs here, never in the shared handle/page models.
use core::arch::asm;

pub(crate) struct NativeInvocationGate;
impl mac_object_table::shared::InvocationGate for NativeInvocationGate
{
    fn run<R>(operation: impl FnOnce() -> R) -> R
    {
        with_masked(operation)
    }
}

/// Keeps a bounded synchronous operation on this hart. Nested entry preserves
/// an already-masked state. The guard and its restoration cannot escape.
pub(crate) fn with_masked<R>(operation: impl FnOnce() -> R) -> R
{
    const MIE: usize = 1 << 3;
    struct Restore(usize);
    impl Drop for Restore
    {
        fn drop(&mut self)
        {
            if self.0 & MIE != 0
            {
                // SAFETY: restores only this hart's previously enabled MIE bit.
                // No nomem: the compiler must not move protected memory access
                // beyond interrupt restoration.
                unsafe {
                    asm!("csrs mstatus, {mask}", mask = in(reg) MIE, options(nostack));
                }
            }
        }
    }
    let previous;
    // SAFETY: System executes in M mode. CSR read/clear is atomic w.r.t. local
    // interrupt entry. The memory clobber prevents hoisting lock access before it.
    unsafe {
        asm!("csrrc {previous}, mstatus, {mask}", previous = out(reg) previous,
                  mask = in(reg) MIE, options(nostack));
    }
    let _restore = Restore(previous);
    operation()
}
