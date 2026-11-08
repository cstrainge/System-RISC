//! PSCI power controller: the AArch64 machine's power mechanism.
//!
//! The machine declares its firmware power interface in its own tree, in the
//! `/psci` node's `method` property. That node describes a function and states
//! no register window, so there is nothing to match or claim: this package
//! declares no device, and it is admitted by initializing. Initialization is
//! therefore the capability test — it resolves the machine's declared conduit
//! and refuses to run unless this mechanism implements it, so the System offers
//! shutdown and restart only where a request would actually be honoured.
//!
//! The request engine is shared with the other machine's power driver; only the
//! mechanism below is this machine's.
#![no_std]
#![deny(unsafe_code)]
use mac_driver::{Completion, Driver, Error, Request, Result, power};
#[cfg(target_arch = "aarch64")]
mod psci;
#[cfg(target_arch = "aarch64")]
mod native
{
    use mac_driver::Client;
    use spinning_top::Spinlock;

    use super::*;

    pub struct PowerDevice;
    pub static DRIVER: PowerDevice = PowerDevice;
    static DEVICE: Spinlock<Option<super::psci::Device>> = Spinlock::new(None);
    impl Driver for PowerDevice
    {
        fn initialize(&self, host: Client) -> Result<()>
        {
            let mut device = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
            if device.is_some()
            {
                return Err(Error(mac_abi::SERVICE_BUSY));
            }
            *device = Some(super::psci::Device::initialize(host)?);
            Ok(())
        }

        fn request(&self, call: Request<'_>, output: &mut [u8]) -> Result<Completion>
        {
            let mut device = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
            power::request(device.as_mut().ok_or(Error(mac_abi::NOT_FOUND))?, call, output)
        }

        fn quiesce(&self) -> Result<()>
        {
            // The conduit owns no claim that System issued, so quiescence is only
            // this module dropping its initialized state. It is stated rather than
            // left to the default so a stopped module can initialize again.
            let mut device = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
            *device = None;
            Ok(())
        }
    }
}
#[cfg(target_arch = "aarch64")]
pub use native::DRIVER;
