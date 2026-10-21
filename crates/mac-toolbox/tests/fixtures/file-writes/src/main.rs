//! Disk-booted private application. Assertions are canonical aggregate test code;
//! this file supplies only the live userspace service table and outcome logging.
#![no_std]
#![no_main]
use core::{fmt::{self, Write},
           sync::atomic::{AtomicPtr, Ordering}};

use mac_toolbox::{System, SystemServices};
#[path = "../../../file_write_cases.rs"]
mod file_write_cases;
static SERVICES: AtomicPtr<SystemServices> = AtomicPtr::new(core::ptr::null_mut());
struct LogBuffer
{
    bytes:  [u8; 512],
    length: usize
}
impl LogBuffer
{
    fn new() -> Self
    {
        Self { bytes:  [0; 512],
               length: 0 }
    }

    fn text(&self) -> &str
    {
        core::str::from_utf8(&self.bytes[..self.length]).unwrap_or("FILE_WRITE_FAIL invalid \
                                                                    diagnostic\n")
    }
}
impl Write for LogBuffer
{
    fn write_str(&mut self, text: &str) -> fmt::Result
    {
        let remaining = self.bytes.len() - self.length;
        let mut n = text.len().min(remaining);
        while !text.is_char_boundary(n)
        {
            n -= 1;
        }
        self.bytes[self.length..self.length + n].copy_from_slice(&text.as_bytes()[..n]);
        self.length += n;
        if n != text.len() { Err(fmt::Error) } else { Ok(()) }
    }
}
/// # Safety
/// The loader supplies a live table that remains valid throughout the application.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> mac_toolbox::abi::Status
{
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(s) => s,
        Err(e) => return e.status()
    };
    SERVICES.store(services.cast_mut(), Ordering::Release);
    let _ = system.log("DESKTOP_READY: durable public File Manager fixture\n");
    match file_write_cases::stage(&system)
    {
        Ok(stage) =>
        {
            let mut text = LogBuffer::new();
            let _ = writeln!(text, "FILE_WRITE_STAGE {stage}");
            let _ = system.log(text.text());
            0
        }
        Err(error) =>
        {
            let mut text = LogBuffer::new();
            let _ = writeln!(text, "FILE_WRITE_FAIL {error}");
            let _ = system.log(text.text());
            -1
        }
    }
}
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> !
{
    let pointer = SERVICES.load(Ordering::Acquire);
    if let Ok(system) = unsafe { System::from_raw(pointer) }
    {
        let mut text = LogBuffer::new();
        let _ = writeln!(text, "FILE_WRITE_FAIL panic: {info}");
        let _ = system.log(text.text());
    }
    loop
    {
        core::hint::spin_loop();
    }
}
