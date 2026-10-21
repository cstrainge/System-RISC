//! Loader boundary for canonical public SDK retained-update assertions.
#![no_std]
#![no_main]
use core::sync::atomic::{AtomicPtr, Ordering};

use mac_toolbox::{System, SystemServices};
#[path = "../../../retained_update_cases.rs"]
mod retained_update_cases;
static SERVICES: AtomicPtr<SystemServices> = AtomicPtr::new(core::ptr::null_mut());

/// # Safety
/// The native loader supplies a valid table for the application's lifetime.
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
    match retained_update_cases::run(&system)
    {
        Ok(()) => 0,
        Err(error) =>
        {
            let _ = system.log("RETAINED_UPDATE_FAIL service\n");
            error.status()
        }
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> !
{
    if let Ok(system) = unsafe { System::from_raw(SERVICES.load(Ordering::Acquire)) }
    {
        let _ = system.log("RETAINED_UPDATE_FAIL assertion\n");
    }
    loop
    {
        core::hint::spin_loop();
    }
}
