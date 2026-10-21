#![no_std]
#![no_main]
use core::fmt::Write;

use mac_toolbox::{System, SystemServices};
struct Message
{
    bytes:  [u8; 128],
    length: usize
}
impl core::fmt::Write for Message
{
    fn write_str(&mut self, text: &str) -> core::fmt::Result
    {
        let end = self.length + text.len();
        self.bytes
            .get_mut(self.length..end)
            .ok_or(core::fmt::Error)?
            .copy_from_slice(text.as_bytes());
        self.length = end;
        Ok(())
    }
}
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
            let mut message = Message { bytes:  [0; 128],
                                        length: 0 };
            let _ = writeln!(&mut message, "STUFFIT_ERROR {error:?} status={}", error.status());
            let _=system.log(core::str::from_utf8(&message.bytes[..message.length]).unwrap_or("STUFFIT_ERROR encoding\n"));
            let _ = system.log("STUFFIT_FAIL\n");
            error.status()
        }
    }
}
