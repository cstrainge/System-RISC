//! Named virtio-console byte streams. This adapter admits only modern coherent
//! MMIO controllers advertising two port slots (`max_ports=2`), with one named
//! `virtserialport,nr=1`. Each module supplies its own pinned four-queue arena; no controller or
//! ring is shared.
#![no_std]
#![deny(unsafe_code)]
pub mod control;
pub mod discovery;
#[cfg(any(target_arch = "riscv64", test))]
mod lifetime;
#[cfg(test)]
extern crate std;
#[cfg(target_arch = "riscv64")]
mod machine;
use mac_driver::Result;
#[cfg(target_arch = "riscv64")]
pub use machine::{Arena, Device};

pub const FRAGMENT: usize = 4096;
/// QEMU reserves slot 0 for virtconsole; the declared single-stream profile uses slot 1.
pub const PORT_ID: u32 = 1;
pub const PORT_SLOTS: u32 = 2;
pub const QUEUE_INDICES: [u32; 4] = [4, 5, 2, 3];
/// MULTIPORT is necessary for named-port controls; VERSION_1 selects the modern
/// transport. QEMU only offers MULTIPORT when max_ports is greater than one.
pub fn validate_profile(features: u64, max_ports: u32) -> core::result::Result<(), Diagnostic>
{
    let required = (1u64 << 1) | (1u64 << 32);
    if features & required != required || max_ports != PORT_SLOTS
    {
        Err(Diagnostic::Profile)
    }
    else
    {
        Ok(())
    }
}
pub const FILESYSTEM_PORT: &str = "org.system-risc.host-filesystem";
pub const VDAGENT_PORT: &str = "com.redhat.spice.0";

/// Each method makes bounded progress and retains no caller slice. A zero-byte
/// transfer is backpressure. Inspect poll/connection generation for disconnects.
pub trait Channel
{
    fn monotonic_microseconds(&self) -> u64;
    fn poll(&mut self) -> Result<bool>;
    fn send(&mut self, bytes: &[u8]) -> Result<usize>;
    fn receive(&mut self, bytes: &mut [u8]) -> Result<usize>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Diagnostic
{
    Discovering,
    DiscoveryTimeout,
    Ready,
    NoController,
    Claimed,
    Profile,
    NameMismatch,
    NameTimeout,
    MalformedControl,
    Disconnected,
    Hardware,
    Cleanup,
    EpochExhausted
}
impl Diagnostic
{
    pub const fn message(self) -> &'static str
    {
        match self
        {
            Self::Discovering => "Discovering the declared named virtio-console port",
            Self::DiscoveryTimeout =>
            {
                "Named virtio-console discovery timed out; check the declared controller name and \
                 profile, or restart System"
            }
            Self::Ready => "The declared named virtio-console port is open",
            Self::NoController =>
            {
                "No compatible virtio-console controller is present; enable the declared named \
                 controller and restart System"
            }
            Self::Claimed =>
            {
                "Compatible virtio-console controllers are already claimed; release the other \
                 owner or restart System"
            }
            Self::Profile =>
            {
                "Unsupported virtio-console profile: configure a dedicated modern coherent MMIO \
                 controller with max_ports=2 and one named virtserialport at nr=1"
            }
            Self::NameMismatch =>
            {
                "No controller advertises the required virtio-console port name; configure its \
                 exact name and restart System"
            }
            Self::NameTimeout =>
            {
                "Virtio-console host did not advertise a port name within five seconds; check the \
                 named virtserialport and restart System"
            }
            Self::MalformedControl =>
            {
                "Malformed or out-of-order virtio-console control notification; reset the host \
                 channel or restart System"
            }
            Self::Disconnected =>
            {
                "The named virtio-console host closed or removed its port; queued bytes were \
                 revoked; waiting for the host to reconnect"
            }
            Self::Hardware =>
            {
                "Virtio-console hardware or DMA ring validation failed; restart System and inspect \
                 the host configuration"
            }
            Self::Cleanup =>
            {
                "Virtio-console reset, DMA unmap or claim release remains incomplete; pinned \
                 storage is retained"
            }
            Self::EpochExhausted => "Virtio-console connection generation exhausted; restart System"
        }
    }
}
