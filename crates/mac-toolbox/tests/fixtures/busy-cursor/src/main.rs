//! Loader boundary for canonical public SDK busy-cursor assertions.
#![no_std]
#![no_main]
use core::sync::atomic::{AtomicPtr, Ordering};

use mac_toolbox::{Status, System, SystemServices};
#[path = "../../../busy_cursor_cases.rs"]
mod busy_cursor_cases;
static SERVICES: AtomicPtr<SystemServices> = AtomicPtr::new(core::ptr::null_mut());

fn clock() -> u64
{
    let ticks;
    // Read the monotonic hardware counter without a Toolbox call or event poll.
    unsafe {
        core::arch::asm!("rdtime {}", out(reg) ticks, options(nomem, nostack));
    }
    ticks
}

/// # Safety
/// The native loader supplies a valid table for the application's lifetime.
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
    match busy_cursor_cases::run(&system, clock)
    {
        Ok(()) => 0,
        Err(error) =>
        {
            let _ = system.log("BUSY_CURSOR_FAIL service\n");
            error
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> !
{
    if let Ok(system) = unsafe { System::from_raw(SERVICES.load(Ordering::Acquire)) }
    {
        let _ = system.log("BUSY_CURSOR_FAIL assertion\n");
    }
    loop
    {
        core::hint::spin_loop();
    }
}
