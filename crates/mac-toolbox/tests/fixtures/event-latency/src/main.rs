//! Loader boundary only; acceptance assertions live in public SDK tests.
#![no_std]
#![no_main]
#![allow(dead_code)]
use core::{fmt::{self, Write},
           sync::atomic::{AtomicPtr, Ordering}};

use mac_toolbox::{Status, System, SystemServices};
#[path = "../../../event_latency_cases.rs"]
mod cases;
static SERVICES: AtomicPtr<SystemServices> = AtomicPtr::new(core::ptr::null_mut());
pub struct LogBuffer
{
    bytes:  [u8; 768],
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
        Self { bytes:  [0; 768],
               length: 0 }
    }

    pub fn text(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.length]).unwrap_or("EVENT_LATENCY_FAIL utf8\n")
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
/// # Safety
/// The loader supplies a live application service table.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> Status
{
    SERVICES.store(services.cast_mut(), Ordering::Release);
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(system) => system,
        Err(error) => return error
    };
    match cases::run(&system)
    {
        Ok(()) => 0,
        Err(error) =>
        {
            let mut text = LogBuffer::new();
            let _ = writeln!(text, "EVENT_LATENCY_FAIL {error:?}");
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
        let _ = writeln!(text, "EVENT_LATENCY_FAIL panic {info}");
        let _ = system.log(text.text());
    }
    loop
    {
        core::hint::spin_loop();
    }
}
