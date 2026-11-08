//! The AArch64 runtime owns TPIDR_EL0 while ELF TLS is disabled for this
//! target. It is the software thread pointer register: EL0 reads and writes it
//! directly, and it survives a switch to EL1, which is why the native trap frame
//! carries it. System's own control block lives in TPIDR_EL1 and never aliases
//! this value.

#[inline]
pub(super) fn thread_pointer() -> *mut ()
{
    let pointer;
    unsafe {
        core::arch::asm!("mrs {value}, tpidr_el0", value = out(reg) pointer,
                         options(nomem, nostack));
    }
    pointer
}

/// The caller owns this actual thread's runtime pointer and its live storage.
#[inline]
pub(super) unsafe fn set_thread_pointer(pointer: *mut ())
{
    unsafe {
        core::arch::asm!("msr tpidr_el0, {value}", value = in(reg) pointer, options(nostack));
    }
}
