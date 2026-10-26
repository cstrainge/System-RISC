//! Return an ordinary initialization error after publishing real pending work.
#![no_std]
#![no_main]
use mac_driver::{ClassId, Client, Completion, Driver, Error, Phase, Request, Result};
struct Device;
mac_driver::export_driver!(Device);
mac_driver::driver_panic_handler!();
impl Driver for Device
{
    fn initialize(&self, host: Client) -> Result<()>
    {
        let provider = host.resolve(ClassId(*b"TEST"), 1)?;
        let operation = if cfg!(feature = "no-ack")
        {
            9
        }
        else if cfg!(feature = "reply")
        {
            10
        }
        else
        {
            8
        };
        let mut ticket = 0;
        let mut output = [0; 16];
        if provider.invoke(&mut ticket, Phase::Start, operation, &[], &mut output)?
           != Completion::Pending
           || ticket == 0
        {
            return Err(Error(mac_abi::BAD_ABI));
        }
        Err(Error(mac_abi::BAD_ARGUMENT))
    }

    fn request(&self, _: Request<'_>, _: &mut [u8]) -> Result<Completion>
    {
        Err(Error(mac_abi::NOT_SUPPORTED))
    }
}
