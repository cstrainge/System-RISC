//! Entry/outcome transport. Assertions belong to the public SDK test source.
#![no_std]
#![no_main]
use mac_toolbox::{System, SystemServices};
#[path = "../../../host_filesystem_exit_case.rs"]
mod cases;
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> i32
{
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(system) => system,
        Err(error) => return error.status()
    };
    match cases::check(&system)
    {
        Ok(()) => 0,
        Err(error) =>
        {
            let _ = system.log("HOST_FS_EXIT_WORKER_FAIL\n");
            error.status()
        }
    }
}
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> !
{
    loop
    {
        core::hint::spin_loop();
    }
}
