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
        let devices = mac_driver::devices::DeviceService::new(host)?;
        for ordinal in 0..8
        {
            match devices.probe(ordinal)
            {
                Ok(info) if info.key[..4] == 2u32.to_le_bytes() =>
                {
                    // This fixture declares the bus match, but firmware still
                    // owns boot storage. Resident ownership is an independent
                    // admission rule and must reject the real service call.
                    if info.flags & mac_driver::devices::interface::RESIDENT_OWNED == 0
                       || devices.claim(ordinal) != Err(Error(-12))
                    { return Err(Error(-1)); }
                    return Ok(());
                }
                Ok(_) | Err(Error(-3)) => (),
                Err(error) => return Err(error),
            }
        }
        Err(Error(-3))
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
