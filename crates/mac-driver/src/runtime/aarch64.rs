//! Audited trap/register boundary for the allocation-free driver panic
//! scaffold. The trap number travels in x8 and the argument pair in x0/x1.
pub(super) fn log(bytes: &[u8])
{
    let mut call = mac_abi::traps::LogCall { bytes:  bytes.as_ptr(),
                                             length: bytes.len() };
    let mut result = core::ptr::from_mut(&mut call).addr();
    // SAFETY: this synchronous svc borrows initialized stack state and a live
    // immutable UTF-8 slice for the duration of the call. No pointer escapes.
    unsafe {
        core::arch::asm!("svc #0", in("x8") mac_abi::traps::LOG,
            inlateout("x0") result, in("x1") core::mem::size_of_val(&call),
            clobber_abi("C"));
    }
    let _ = result;
}
pub(super) fn fault() -> !
{
    // SAFETY: an explicit breakpoint enters the System's reported
    // privileged-fault shutdown. It never resumes the broken module.
    unsafe {
        core::arch::asm!("brk #0", options(noreturn));
    }
}

/// Private worker switch trap. All wire/register access stays in this binding.
pub(super) fn worker(call: &mut crate::abi::WorkerCall) -> mac_abi::Status
{
    let mut result = core::ptr::from_mut(call).addr();
    // SAFETY: exact initialized stack record remains live and exclusive across
    // the native switch. The scheduler pins this actual worker stack until it
    // resumes or exits; trap authentication rejects other callers.
    unsafe {
        core::arch::asm!("svc #0", in("x8") crate::abi::worker::TRAP,
            inlateout("x0") result, in("x1") core::mem::size_of::<crate::abi::WorkerCall>(),
            clobber_abi("C"));
    }
    result as mac_abi::Status
}
