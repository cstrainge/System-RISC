//! The RV64 runtime owns tp while ELF TLS is disabled for this target.

#[inline]
pub(super) fn thread_pointer() -> *mut ()
{
    let pointer;
    unsafe {
        core::arch::asm!("mv {value}, tp", value = out(reg) pointer,
                         options(nomem, nostack));
    }
    pointer
}

/// The caller owns this actual thread's runtime pointer and its live storage.
#[inline]
pub(super) unsafe fn set_thread_pointer(pointer: *mut ())
{
    unsafe {
        core::arch::asm!("mv tp, {value}", value = in(reg) pointer, options(nostack));
    }
}
