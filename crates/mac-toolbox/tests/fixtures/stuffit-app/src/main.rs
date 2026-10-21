#![no_std]
#![no_main]
use mac_toolbox::{System, SystemServices};
#[path = "../../../stuffit_cases.rs"]
mod cases;
mac_toolbox::panic_handler!();
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> i32
{
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(system) => system,
        Err(error) => return error.status()
    };
    let _ = system.log("DESKTOP_READY: public StuffIt fixture\n");
    match cases::check(&system)
    {
        Ok(()) =>
        {
            let _ = system.log("STUFFIT_PASS\n");
            0
        }
        Err(error) =>
        {
            let _ = system.log("STUFFIT_FAIL\n");
            error.status()
        }
    }
}
