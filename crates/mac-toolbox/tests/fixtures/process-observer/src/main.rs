//! Only the loader boundary is unsafe; canonical assertions use the public SDK.
#![no_std]
#![no_main]
#![allow(dead_code)]
use core::{fmt::{self, Write},
           sync::atomic::{AtomicPtr, Ordering}};

use mac_toolbox::{System, SystemServices};
#[path = "../../../process_cases.rs"]
mod process_cases;
#[path = "../../../process_pascal_cases.rs"]
mod process_pascal_cases;
#[cfg(feature = "pressure")]
mod pressure;
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
    #[cfg(feature = "pressure")]
    let outcome = pressure::run(&system);
    #[cfg(not(feature = "pressure"))]
    let outcome = {
        // SAFETY: the same loader-validated table supplies the raw legacy conversion gate.
        let gate = unsafe { (*services).trap };
        process_pascal_cases::current_name(&system, gate).and_then(|_| {
            process_cases::observer(&system, |id| {
                process_pascal_cases::rejected_id(&system, gate, id)
            })
        })
    };
    match outcome
    {
        Ok(()) => 0,
        Err(error) =>
        {
            let mut text = LogBuffer::new();
            let _ = writeln!(text,
                             "{} {error:?}",
                             if cfg!(feature = "pressure")
                             {
                                 "PRESSURE_ACTOR_FAIL"
                             }
                             else
                             {
                                 "PROCESS_FAIL"
                             });
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
        let _ = writeln!(text,
                         "{} panic {info}",
                         if cfg!(feature = "pressure")
                         {
                             "PRESSURE_ACTOR_FAIL"
                         }
                         else
                         {
                             "PROCESS_FAIL"
                         });
        let _ = system.log(text.text());
    }
    loop
    {
        core::hint::spin_loop();
    }
}
