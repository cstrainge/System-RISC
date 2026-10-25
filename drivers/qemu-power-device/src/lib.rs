//! QEMU power controller. The engine is safe Rust; MMIO lives in one adapter.
#![no_std]
#![deny(unsafe_code)]
use mac_driver::{Completion, Error, Phase, Request, Result};
use mac_power_interface::{Action, CLASS};
pub trait Hardware
{
    fn trigger(&mut self, action: Action) -> Result<()>;
}
pub fn request(device: &mut impl Hardware, request: Request<'_>, output: &mut [u8])
               -> Result<Completion>
{
    if request.class != CLASS
    {
        return Err(Error(mac_abi::NOT_SUPPORTED));
    }
    if request.phase != Phase::Start || !request.input.is_empty() || !output.is_empty()
    {
        return Err(Error(mac_abi::BAD_ARGUMENT));
    }
    let action = Action::from_operation(request.operation).ok_or(Error(mac_abi::NOT_SUPPORTED))?;
    device.trigger(action)?;
    Ok(Completion::Complete(0))
}
#[cfg(target_arch = "riscv64")]
mod machine;
#[cfg(target_arch = "riscv64")]
mod native
{
    use mac_driver::{Client, Driver};
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
            super::request(device.as_mut().ok_or(Error(mac_abi::NOT_FOUND))?, call, output)
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
