#![no_std]
#![no_main]
use mac_driver::{ClassId, Client, Completion, Driver, Error, Phase, Request, Result};
struct Device;
static DEVICE: Device = Device;
mac_driver::export_driver!(DEVICE);
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> !
{
    loop
    {
        core::hint::spin_loop();
    }
}
impl Driver for Device
{
    fn initialize(&self, _: Client) -> Result<()>
    {
        Ok(())
    }

    fn request(&self, call: Request<'_>, output: &mut [u8]) -> Result<Completion>
    {
        if call.class != ClassId(*b"TEST")
           || call.operation != 7
           || call.phase != Phase::Start
           || output.len() < call.input.len()
        {
            return Err(Error(-2));
        }
        for (target, source) in output.iter_mut().zip(call.input.iter().rev())
        {
            *target = *source;
        }
        Ok(Completion::Complete(call.input.len()))
    }
}
