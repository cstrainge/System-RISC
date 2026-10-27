//! Loader boundary; the native acceptance assertions live with public SDK tests.
#![no_std]
#![no_main]
use core::sync::atomic::{AtomicPtr, Ordering};

use mac_toolbox::{System, SystemServices};
#[path = "../../../crash_paint_refusal_cases.rs"]
mod cases;
#[path = "../../diagnostics.rs"]
mod diagnostics;
static SERVICES: AtomicPtr<SystemServices> = AtomicPtr::new(core::ptr::null_mut());
/// # Safety
/// Loader supplies a valid services table for this application's lifetime.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> mac_toolbox::abi::Status
{
    SERVICES.store(services.cast_mut(), Ordering::Release);
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(system) => system,
        Err(error) => return error.status()
    };
    match cases::run(&system)
    {
        Ok(()) => 0,
        Err(error) =>
        {
            diagnostics::error(&system, "CRASH_PAINT_FAIL service", error);
            error.status()
        }
    }
}
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> !
{
    if let Ok(system) = unsafe { System::from_raw(SERVICES.load(Ordering::Acquire)) }
    {
        cases::assertion_failed(&system, info);
    }
    loop
    {
        core::hint::spin_loop();
    }
}
