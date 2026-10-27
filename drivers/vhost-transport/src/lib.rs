//! The host byte transport owns frame progress and hardware buffers. Filesystem
//! semantics remain in the separate filesystem provider. Ordinary engine logic
//! is safe Rust; the machine module is the audited MMIO/DMA boundary.
#![no_std]
#![deny(unsafe_code)]
pub mod engine;

#[cfg(target_arch = "riscv64")]
mod driver
{
    use mac_abi::{NOT_SUPPORTED, SERVICE_BUSY};
    use mac_driver::{Client, Completion, Driver, Error, Request, Result};
    use mac_virtio_console::{Arena, Device, FILESYSTEM_PORT};
    use spinning_top::Spinlock;

    use crate::engine::{Buffers, Engine};
    static ARENA: Arena = Arena::new();
    struct State
    {
        engine: Engine,
        device: Option<Device>
    }
    static STATE: Spinlock<State> = Spinlock::new(State { engine: Engine::new(),
                                                          device: None });
    // Separate zero-only byte storage keeps packet buffers in BSS rather than
    // embedding hundreds of kilobytes of zeros around an Option's niche tag.
    static BUFFERS: Spinlock<Buffers> = Spinlock::new(Buffers::new());
    pub struct Transport;
    pub static DRIVER: Transport = Transport;
    impl Driver for Transport
    {
        fn initialize(&self, host: Client) -> Result<()>
        {
            let mut state = STATE.try_lock().ok_or(Error(SERVICE_BUSY))?;
            if state.device.is_some()
            {
                return Err(Error(SERVICE_BUSY));
            }
            state.device = Some(Device::initialize(host, &ARENA, FILESYSTEM_PORT)?);
            Ok(())
        }

        fn request(&self, request: Request<'_>, output: &mut [u8]) -> Result<Completion>
        {
            let mut state = STATE.try_lock().ok_or(Error(SERVICE_BUSY))?;
            let State { engine,
                        device } = &mut *state;
            let mut buffers = BUFFERS.try_lock().ok_or(Error(SERVICE_BUSY))?;
            engine.request(device.as_mut().ok_or(Error(NOT_SUPPORTED))?,
                           request,
                           output,
                           &mut buffers)
        }

        fn quiesce(&self) -> Result<()>
        {
            let mut state = STATE.try_lock().ok_or(Error(SERVICE_BUSY))?;
            state.device.as_mut().ok_or(Error(NOT_SUPPORTED))?.quiesce()
        }
    }
}
#[cfg(target_arch = "riscv64")]
pub use driver::DRIVER;
