#![no_std]
#![deny(unsafe_code)]
pub mod engine;
#[cfg(target_arch = "riscv64")]
mod driver
{
    use mac_abi::{NOT_SUPPORTED, SERVICE_BUSY};
    use mac_driver::{Client, Completion, Driver, Error, Request, Result};
    use mac_virtio_console::{Arena, Device, VDAGENT_PORT};
    use spinning_top::Spinlock;
    static ARENA: Arena = Arena::new();
    static DEVICE: Spinlock<Option<Device>> = Spinlock::new(None);
    pub struct Transport;
    pub static DRIVER: Transport = Transport;
    impl Driver for Transport
    {
        fn initialize(&self, host: Client) -> Result<()>
        {
            let mut device = DEVICE.try_lock().ok_or(Error(SERVICE_BUSY))?;
            if device.is_some()
            {
                return Err(Error(SERVICE_BUSY));
            }
            *device = Some(Device::initialize(host, &ARENA, VDAGENT_PORT)?);
            Ok(())
        }

        fn request(&self, request: Request<'_>, output: &mut [u8]) -> Result<Completion>
        {
            let mut device = DEVICE.try_lock().ok_or(Error(SERVICE_BUSY))?;
            crate::engine::request(device.as_mut().ok_or(Error(NOT_SUPPORTED))?, request, output)
        }

        fn quiesce(&self) -> Result<()>
        {
            DEVICE.try_lock()
                  .ok_or(Error(SERVICE_BUSY))?
                  .as_mut()
                  .ok_or(Error(NOT_SUPPORTED))?
                  .quiesce()
        }
    }
}
#[cfg(target_arch = "riscv64")]
pub use driver::DRIVER;
