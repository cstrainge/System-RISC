//! Only entry/outcome transport; all acceptance assertions live in the parent
//! Toolbox test directory and call the public userspace SDK.
#![no_std]
#![no_main]
use mac_toolbox::{System, SystemServices};
#[path = "../../../host_filesystem_cases.rs"]
mod cases;
#[path = "../../diagnostics.rs"]
mod diagnostics;
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start(services: *const SystemServices) -> i32
{
    let system = match unsafe { System::from_raw(services) }
    {
        Ok(system) => system,
        Err(error) => return error.status()
    };
    let _ = system.log("DESKTOP_READY: public host filesystem fixture\n");
    let gate = unsafe { (*services).trap };
    let result = if cfg!(any(feature = "disconnect-source", feature = "disconnect-destination"))
    {
        cases::check_disconnect(&system, cfg!(feature = "disconnect-source"))
    }
    else if cfg!(feature = "metadata-restore")
    {
        cases::check_metadata_restored(&system)
    }
    else
    {
        cases::check(&system, gate).and_then(|()| cases::check_legacy_writer(&system, gate))
    };
    match result
    {
        Ok(()) =>
        {
            let _ = system.log(if cfg!(feature = "metadata-restore")
                               {
                                   "HOST_METADATA_RESTORE_PASS\n"
                               }
                               else
                               {
                                   "HOST_FILESYSTEM_PASS\n"
                               });
            0
        }
        Err(error) =>
        {
            diagnostics::error(&system, "HOST_FILESYSTEM_ERROR", error);
            let _ = system.log("HOST_FILESYSTEM_FAIL\n");
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
