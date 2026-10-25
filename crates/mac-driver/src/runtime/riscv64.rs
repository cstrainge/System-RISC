//! Audited trap/register boundary for the allocation-free driver panic scaffold.
pub(super) fn log(bytes: &[u8])
{
    let mut call = mac_abi::traps::LogCall { bytes:  bytes.as_ptr(),
                                             length: bytes.len() };
    let mut result = core::ptr::from_mut(&mut call).addr();
    // SAFETY: this synchronous trap borrows initialized stack state and a live
    // immutable UTF-8 slice for the duration of the call. No pointer escapes.
    unsafe {
        core::arch::asm!("ecall",in("a7") mac_abi::traps::LOG,
            inlateout("a0") result,in("a1") core::mem::size_of_val(&call),clobber_abi("C"));
    }
    let _ = result;
}
pub(super) fn fault() -> !
{
    // SAFETY: an explicit illegal instruction enters the System's existing
    // reported privileged-fault shutdown. It never resumes the broken module.
    unsafe {
        core::arch::asm!("unimp", options(noreturn));
    }
}
