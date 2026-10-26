//! Loader boundary; behavioral assertions live in canonical public SDK cases.
#![no_std]
#![no_main]
use mac_toolbox::{System, SystemServices};
#[path = "../../../finder_recovery_cases.rs"]
pub mod cases;
mac_toolbox::panic_handler!();
/// # Safety
/// The loader supplies a validated service table for the application's lifetime.
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> mac_toolbox::abi::Status
{
    if let Err(status) = unsafe { mac_toolbox::panic::install(services) }
    {
        return status;
    }
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(system) => system,
        Err(error) => return error.status()
    };
    match cases::worker(&system)
    {
        Ok(()) => 0,
        Err(error) =>
        {
            let _ = system.log("FINDER_RECOVERY_FAIL service\n");
            error.status()
        }
    }
}
