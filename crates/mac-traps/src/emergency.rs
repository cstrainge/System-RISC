//! One audited RV64 last-resort boundary. Used only when service installation
//! is absent or the genuine nonreturning process EXIT unexpectedly returns.
use core::fmt::{self, Write};

use mac_abi as abi;
struct Text
{
    bytes: [u8; 768],
    used:  usize
}
impl Write for Text
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        if text.len() > self.bytes.len() - self.used
        {
            return Err(fmt::Error);
        }
        self.bytes[self.used..self.used + text.len()].copy_from_slice(text.as_bytes());
        self.used += text.len();
        Ok(())
    }
}
unsafe fn invoke<T>(number: u32, request: &mut T) -> i64
{
    let mut result = core::ptr::from_mut(request).addr();
    // SAFETY: exact synchronous native request storage stays live through the
    // trap. System authenticates the actual executor; no AppId is supplied.
    unsafe {
        core::arch::asm!("ecall", in("a7") number, inlateout("a0") result,
                         in("a1") core::mem::size_of::<T>(), clobber_abi("C"));
    }
    result as i64
}
/// Emit a bounded diagnostic and request terminal application exit without
/// depending on an installed services table, allocation, TLS or user callbacks.
pub fn abort(arguments: fmt::Arguments<'_>) -> !
{
    let mut text = Text { bytes: [0; 768],
                          used:  0 };
    if writeln!(text, "{arguments}").is_err()
    {
        text.used = 0;
        let _ = text.write_str("APPLICATION_RUNTIME_CONTRACT_FAILURE diagnostic overflow\n");
    }
    let mut log = abi::traps::LogCall { bytes:  text.bytes.as_ptr(),
                                        length: text.used };
    // SAFETY: fixed live stack buffers; no service-table or heap dependency.
    let _ = unsafe { invoke(abi::traps::LOG, &mut log) };
    let mut exit = abi::processes::NativeExitCall { code:     101,
                                                    flags:    abi::processes::NATIVE_EXIT_ABORT,
                                                    reserved: 0 };
    let status = unsafe { invoke(abi::traps::PROCESS, &mut exit) };
    exit_returned(status)
}
/// Last-resort terminal fault when the resident nonreturning exit contract fails.
pub fn exit_returned(status: i64) -> !
{
    let mut text = Text { bytes: [0; 768],
                          used:  0 };
    let _ = writeln!(text,
                     "APPLICATION_EXIT_RETURNED status={status}; deliberate terminal fault, not \
                      contained recovery");
    let mut log = abi::traps::LogCall { bytes:  text.bytes.as_ptr(),
                                        length: text.used };
    let _ = unsafe { invoke(abi::traps::LOG, &mut log) };
    // SAFETY: EXIT violated its nonreturning contract. An explicit architecture
    // fault follows the System's loud terminal path; it is never a silent hang
    // or a claim of arbitrary privileged-fault containment (M-D04).
    unsafe {
        core::arch::asm!("unimp", options(noreturn, nostack));
    }
}
