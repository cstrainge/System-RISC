//! Native process fixture variants; heap mode uses the platform application scaffold.
#![no_std]
#![no_main]
#![allow(dead_code)]
#![cfg_attr(feature = "heap", feature(allocator_ext, vec_fallible_shrink))]
#[cfg(all(feature = "heap", feature = "smp"))]
compile_error!("heap and smp are distinct native workloads; choose only one");
#[cfg(any(feature = "heap", feature = "tls"))]
extern crate alloc;
use core::fmt::{self, Write};
#[cfg(not(any(feature = "heap", feature = "tls")))]
use core::sync::atomic::{AtomicPtr, Ordering};

#[cfg(not(any(feature = "heap", feature = "tls")))]
use mac_toolbox::{System, SystemServices};
#[cfg(not(any(feature = "heap", feature = "tls")))]
#[path = "../../../process_cases.rs"]
mod process_cases;
#[cfg(feature = "smp")]
mod smp;
#[cfg(feature = "heap")]
mod heap;
#[cfg(feature = "tls")]
mod tls;
#[cfg(feature = "tls")]
mod address_wait;
#[cfg(feature = "tls")]
mac_toolbox::application!(tls::run);
#[cfg(any(all(feature = "tls", feature = "heap"), all(feature = "tls", feature = "smp")))]
compile_error!("tls, heap and smp are distinct native workloads; choose one");
#[cfg(feature = "heap")]
mac_toolbox::application!(heap::run);
#[cfg(not(any(feature = "heap", feature = "tls")))]
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
#[cfg(not(any(feature = "heap", feature = "tls")))]
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
#[cfg(not(any(feature = "heap", feature = "tls")))]
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
#[cfg(not(any(feature = "heap", feature = "tls")))]
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
