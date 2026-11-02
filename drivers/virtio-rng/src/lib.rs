//! External VirtIO entropy provider. Hardware and its retained DMA state belong
//! to this package; resident code selects ENTR by interface, never this filename.
#![no_std]
#![deny(unsafe_code)]

#[cfg(target_arch = "riscv64")]
mod machine;

#[cfg(target_arch = "riscv64")]
mod driver
{
    use mac_driver::{Client, Completion, Driver, Error, Request, Result};
    use spinning_top::Spinlock;

    use super::machine::Device;

    static DEVICE: Spinlock<Option<Device>> = Spinlock::new(None);
    pub struct Entropy;
    pub static DRIVER: Entropy = Entropy;

    impl Driver for Entropy
    {
        fn initialize(&self, host: Client) -> Result<()>
        {
            let mut slot = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
            if slot.is_some()
            {
                return Err(Error(mac_abi::SERVICE_BUSY));
            }
            // Construction acquires no claim or DMA mapping. Once published,
            // only successful quiescence may remove this retained state.
            *slot = Some(Device::new(host)?);
            Ok(())
        }

        fn request(&self, request: Request<'_>, output: &mut [u8]) -> Result<Completion>
        {
            DEVICE.try_lock()
                  .ok_or(Error(mac_abi::SERVICE_BUSY))?
                  .as_mut()
                  .ok_or(Error(mac_abi::NOT_FOUND))?
                  .request(request, output)
        }

        fn quiesce(&self) -> Result<()>
        {
            let mut slot = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
            if let Some(device) = slot.as_mut()
            {
                device.quiesce()?;
            }
            *slot = None;
            Ok(())
        }
    }
}

#[cfg(target_arch = "riscv64")]
pub use driver::DRIVER;
