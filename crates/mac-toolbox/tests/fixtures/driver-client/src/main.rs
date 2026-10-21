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
    fn initialize(&self, host: Client) -> Result<()>
    {
        let provider = host.resolve(ClassId(*b"TEST"), 1)?;
        let mut output = [0; 32];
        let mut ticket = 0;
        let result = provider.invoke(&mut ticket, Phase::Start, 7, b"class-neutral", &mut output)?;
        if result != Completion::Complete(13) || &output[..13] != b"lartuen-ssalc" || ticket != 0
        {
            return Err(Error(-1));
        }
        Ok(())
    }

    fn request(&self, _: Request<'_>, _: &mut [u8]) -> Result<Completion>
    {
        Err(Error(-4))
    }
}
