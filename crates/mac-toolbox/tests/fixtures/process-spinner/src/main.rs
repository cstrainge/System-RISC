//! Only the loader boundary is unsafe; canonical assertions use the public SDK.
#![no_std]
#![no_main]
#![allow(dead_code)]
use core::{fmt::{self, Write},
           sync::atomic::{AtomicPtr, Ordering}};

use mac_toolbox::{System, SystemServices};
#[path = "../../../process_cases.rs"]
mod process_cases;
#[cfg(feature = "smp")]
mod smp;
static SERVICES: AtomicPtr<SystemServices> = AtomicPtr::new(core::ptr::null_mut());
pub struct LogBuffer
{
    bytes:  [u8; 384],
    length: usize
}
impl Default for LogBuffer
{
    fn default() -> Self
    {
        Self::new()
    }
}
impl LogBuffer
{
    pub fn new() -> Self
    {
        Self { bytes:  [0; 384],
               length: 0 }
    }

    pub fn text(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.length]).unwrap_or("PROCESS_FAIL invalid \
                                                                    diagnostic\n")
    }
}
impl Write for LogBuffer
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        if text.len() > self.bytes.len() - self.length
        {
            return Err(fmt::Error);
        }
        self.bytes[self.length..self.length + text.len()].copy_from_slice(text.as_bytes());
        self.length += text.len();
        Ok(())
    }
}
fn clock() -> u64
{
    let ticks;
    // SAFETY: rdtime reads the QEMU RISC-V monotonic counter and touches no memory.
    unsafe {
        core::arch::asm!("rdtime {}", out(reg) ticks, options(nomem, nostack));
    }
    ticks
}

/// # Safety
/// The loader supplies a valid service table for this application's lifetime.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> mac_toolbox::abi::Status
{
    SERVICES.store(services.cast_mut(), Ordering::Release);
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(s) => s,
        Err(e) => return e.status()
    };
    #[cfg(feature = "smp")]
    let outcome = smp::run(&system);
    #[cfg(not(feature = "smp"))]
    let outcome = process_cases::spinner(&system, clock);
    match outcome
    {
        Ok(()) => 0,
        Err(error) =>
        {
            let mut text = LogBuffer::new();
            let _ = writeln!(text, "PROCESS_FAIL {error:?}");
            let _ = system.log(text.text());
            -1
        }
    }
}
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> !
{
    if let Ok(system) = unsafe { System::from_raw(SERVICES.load(Ordering::Acquire)) }
    {
        let mut text = LogBuffer::new();
        let _ = writeln!(text, "PROCESS_FAIL panic {info}");
        let _ = system.log(text.text());
    }
    loop
    {
        core::hint::spin_loop();
    }
}
