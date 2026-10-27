//! Bounded real INIT entry used only for the private startup artwork gate.
#![no_std]
#![no_main]
use mac_driver::{Client, Completion, Driver, Error, Request, Result};
struct Probe;
mac_driver::export_driver!(Probe);
mac_driver::driver_panic_handler!();
impl Driver for Probe
{
    fn initialize(&self, _: Client) -> Result<()>
    {
        if cfg!(feature = "refusal") { Err(Error(-4)) } else { Ok(()) }
    }

    fn request(&self, _: Request<'_>, _: &mut [u8]) -> Result<Completion>
    {
        Err(Error(-4))
    }
}
