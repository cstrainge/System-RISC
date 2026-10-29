//! Existing native fixture transport: one audited LOG/clock/negative-ABI edge.
//! Worker bodies use safe SDK interfaces; no synthetic thread/account is issued.
use core::{fmt::{Arguments, Write},
           ptr,
           sync::atomic::{AtomicPtr, Ordering}};

use mac_driver::abi;
#[cfg(not(feature = "pressure"))]
use mac_driver::{Error, Result};
static HOST: AtomicPtr<abi::Host> = AtomicPtr::new(ptr::null_mut());
pub unsafe fn install(host: *const abi::Host)
{
    // The actual loader owns this immutable descriptor for this module's life.
    HOST.store(host.cast_mut(), Ordering::Release);
}
pub fn now() -> u64
{
    let ticks;
    // SAFETY: side-effect-free native QEMU monotonic 10 MHz counter.
    unsafe {
        core::arch::asm!("rdtime {}", out(reg) ticks, options(nomem, nostack));
    }
    ticks
}
#[cfg(not(feature = "pressure"))]
pub fn current_token() -> Result<u64>
{
    // SAFETY: real entry installed the immutable resident descriptor.
    let host = unsafe { &*HOST.load(Ordering::Acquire) };
    let callback = host.workers.ok_or(Error(mac_abi::NOT_SUPPORTED))?;
    let mut call = abi::WorkerCall { header:        abi::Header::of::<abi::WorkerCall>(),
                                     operation:     abi::worker::CURRENT,
                                     flags:         0,
                                     worker:        0,
                                     specification: ptr::null(),
                                     state:         0,
                                     exit_status:   0,
                                     microseconds:  0 };
    let status = unsafe { callback(host.context, &mut call) };
    if status != 0
    {
        return Err(Error(status));
    }
    Ok(call.worker)
}
pub fn log(arguments: Arguments<'_>)
{
    struct Text
    {
        bytes:  [u8; 512],
        length: usize
    }
    impl Write for Text
    {
        fn write_str(&mut self, value: &str) -> core::fmt::Result
        {
            if value.len() > self.bytes.len() - self.length
            {
                return Err(core::fmt::Error);
            }
            self.bytes[self.length..self.length + value.len()].copy_from_slice(value.as_bytes());
            self.length += value.len();
            Ok(())
        }
    }
    let mut text = Text { bytes:  [0; 512],
                          length: 0 };
    assert!(text.write_fmt(arguments).is_ok(), "bounded worker outcome diagnostic overflow");
    let mut call = mac_abi::traps::LogCall { bytes:  text.bytes.as_ptr(),
                                             length: text.length };
    let mut result = ptr::from_mut(&mut call).addr();
    // SAFETY: one synchronous native LOG borrows exact live stack/text storage.
    unsafe {
        core::arch::asm!("ecall", in("a7") mac_abi::traps::LOG,
        inlateout("a0") result, in("a1") core::mem::size_of_val(&call), clobber_abi("C"));
    }
    let _ = result;
}
/// Observe a token issued by CURRENT in this same real module; no token is
/// synthesized and the resident still enforces ownership/generation.
#[cfg(not(feature = "pressure"))]
pub fn state(token: u64) -> Result<u32>
{
    let host = unsafe { &*HOST.load(Ordering::Acquire) };
    let callback = host.workers.ok_or(Error(mac_abi::NOT_SUPPORTED))?;
    let mut call = abi::WorkerCall { header:        abi::Header::of::<abi::WorkerCall>(),
                                     operation:     abi::worker::STATUS,
                                     flags:         0,
                                     worker:        token,
                                     specification: ptr::null(),
                                     state:         0,
                                     exit_status:   0,
                                     microseconds:  0 };
    let status = unsafe { callback(host.context, &mut call) };
    if status != 0
    {
        return Err(Error(status));
    }
    Ok(call.state)
}
