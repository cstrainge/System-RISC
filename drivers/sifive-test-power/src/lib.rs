//! SiFive test finisher power controller: the RISC-V `virt` machine's power
//! mechanism.
//!
//! The machine describes the finisher as an MMIO device, so this driver matches
//! it and claims it through the normal device service, then writes the finisher's
//! pass command for shutdown and reset command for restart. Nothing here is an
//! address of its own: the window and its ownership come from the machine's tree
//! and the resident's claim.
//!
//! The request engine is shared with the other machine's power driver; only the
//! mechanism below is this machine's.
#![no_std]
#![deny(unsafe_code)]
use mac_driver::{Completion, Driver, Error, Request, Result};
use mac_driver::power;
#[cfg(target_arch = "riscv64")]
mod machine;
#[cfg(target_arch = "riscv64")]
mod native
{
    use mac_driver::Client;
    use spinning_top::Spinlock;

    use super::*;

    pub struct PowerDevice;
    pub static DRIVER: PowerDevice = PowerDevice;
    static DEVICE: Spinlock<Option<super::machine::Device>> = Spinlock::new(None);
    impl Driver for PowerDevice
    {
        fn initialize(&self, host: Client) -> Result<()>
        {
            let mut device = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
            if device.is_some()
            {
                return Err(Error(mac_abi::SERVICE_BUSY));
            }
            *device = Some(super::machine::Device::initialize(host)?);
            Ok(())
        }

        fn request(&self, call: Request<'_>, output: &mut [u8]) -> Result<Completion>
        {
            let mut device = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
            power::request(device.as_mut().ok_or(Error(mac_abi::NOT_FOUND))?, call, output)
        }

        fn quiesce(&self) -> Result<()>
        {
            let mut device = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
            if let Some(current) = device.as_ref()
            {
                current.release()?;
            }
            *device = None;
            Ok(())
        }
    }
}
#[cfg(target_arch = "riscv64")]
pub use native::DRIVER;
