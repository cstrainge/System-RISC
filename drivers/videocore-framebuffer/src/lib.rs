//! External VideoCore IV framebuffer driver. System selects this driver by its
//! VIDO interface, never by file name, and matches it to the mailbox window the
//! platform record declares.
//!
//! Skeleton only: the device contract, packaging and dispatch are in place, the
//! engine is not. System may discover and offer this driver; every request is
//! refused with `NOT_SUPPORTED` until the engine exists, so no half-built
//! framebuffer can be handed out.
//!
//! TODO(videocore engine): build the engine on `mac-videocore-fb`, which
//! already encodes and decodes the mailbox property channel
//! (`mac_videocore_fb::acquire` returns the base, size and pitch) and exposes
//! the register block as `mac_videocore_fb::registers::Mailbox`, constructed
//! from the platform record's `bcm2835-mailbox` window rather than a constant.
//! The engine then owns the ATTACH/SERVICE/APPLY/COMMIT/ABORT/RELEASE state
//! machine in `mac-video-interface` and publishes the firmware buffer through
//! the Device DMA service, following `drivers/virtio-gpu`. Mode changes are not
//! offered (capability 0): the firmware allocates one buffer and this driver
//! has no reallocation path yet.
#![no_std]
#![deny(unsafe_code)]
use mac_driver::{Client, Completion, Driver, Error, Request, Result};

pub struct VideoCoreFramebuffer;

pub static DRIVER: VideoCoreFramebuffer = VideoCoreFramebuffer;

impl Driver for VideoCoreFramebuffer
{
    fn initialize(&self, _host: Client) -> Result<()>
    {
        Err(Error(mac_abi::NOT_SUPPORTED))
    }

    fn request(&self, _request: Request<'_>, _output: &mut [u8]) -> Result<Completion>
    {
        Err(Error(mac_abi::NOT_SUPPORTED))
    }
}
