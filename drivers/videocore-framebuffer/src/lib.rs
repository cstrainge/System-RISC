//! External VideoCore IV framebuffer driver. System selects this driver by its
//! VIDO interface, never by file name, and matches it to the mailbox window the
//! platform record declares.
//!
//! The firmware owns display bring-up: it allocates one linear framebuffer and
//! scans it out. This driver asks for that buffer over the mailbox property
//! channel (`mac-videocore-fb`), owns the surface System draws into as Device DMA
//! storage, copies damaged rows into the firmware buffer on present, and changes
//! mode by asking the firmware for a new geometry.
//!
//! The engine, the copy plan and the mode bounds are safe Rust and are exercised
//! on the host against a simulated mailbox. Device access lives in one audited
//! adapter (`native`), which is also the only place that issues cache
//! maintenance.
#![no_std]
#![deny(unsafe_code)]
use core::fmt;

use mac_video_interface::Outcome;

pub mod device;
pub mod engine;
pub mod scanout;

#[cfg(target_arch = "aarch64")]
mod native;

pub use device::Device;
pub use engine::Engine;
#[cfg(target_arch = "aarch64")]
pub use native::DRIVER;
pub use scanout::{Allocation, Bounds, Frame, Plan, Platform};

/// Every way this driver refuses a request. A refusal is always named and
/// carries the numbers that caused it; nothing degrades into a silent no-op.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal
{
    /// The requested mode is below the smallest side the record permits.
    BelowDisplayMinimum
    {
        width: u32, height: u32
    },
    /// The requested mode is larger than the record's declared maximum. The
    /// field names the record's own key, so the message can be checked against
    /// `platforms/<name>.json`.
    ExceedsDisplayMaximum
    {
        width: u32, height: u32, field: &'static str, limit: u32
    },
    /// ATTACH arrived for a driver that already owns a surface.
    AlreadyAttached,
    /// The request needs an attached surface and there is none.
    NotAttached,
    /// A mode change is being prepared; presenting the old mode now would use a
    /// pitch the firmware no longer scans out.
    ModeChangePending,
    /// A second APPLY arrived before the first was committed or aborted.
    PreparePending,
    /// The allocation named by COMMIT/ABORT/RELEASE is not the one this driver
    /// is holding for that purpose.
    UnknownAllocation
    {
        allocation: u64
    },
    /// The allocation belongs to a different generation than the request.
    StaleGeneration
    {
        allocation: u64, expected: u64, observed: u64
    },
    /// The firmware or the mailbox transport refused. The library's named cause
    /// is preserved.
    Device(mac_videocore_fb::Error),
    /// The Device DMA service refused the surface storage.
    NoMemory,
    /// The Device DMA service returned a status this driver cannot classify.
    Dma(mac_abi::Status),
    /// The firmware allocated less than one mode's worth of scan-out.
    AllocationTooSmall
    {
        available: u64, needed: u64
    },
    /// The firmware mode could not be restored after a failed preparation.
    RestoreFailed
    {
        underlying: mac_videocore_fb::Error
    },
    /// The system clock service refused a reading.
    Clock
}
impl Refusal
{
    /// The status this refusal becomes on the wire. The wire carries a code, not
    /// a message; the name is reported through `Display` by the adapter's log.
    pub const fn status(self) -> mac_abi::Status
    {
        match self
        {
            Self::BelowDisplayMinimum { .. }
            | Self::ExceedsDisplayMaximum { .. }
            | Self::UnknownAllocation { .. }
            | Self::StaleGeneration { .. }
            | Self::AllocationTooSmall { .. } => mac_abi::BAD_ARGUMENT,
            Self::AlreadyAttached | Self::PreparePending | Self::ModeChangePending =>
            {
                mac_abi::SERVICE_BUSY
            }
            Self::NotAttached => mac_abi::NOT_FOUND,
            Self::Device(_) => mac_abi::IO_ERROR,
            Self::NoMemory => mac_abi::NO_MEMORY,
            Self::Dma(status) => status,
            Self::RestoreFailed { .. } | Self::Clock => mac_abi::SERVICE_BUSY
        }
    }

    /// The typed APPLY result this refusal corresponds to, when the contract has
    /// one. A mode the record does not permit is `TooSmall`/`TooLarge`, not a
    /// transport error, so System can report it as a mode refusal.
    pub const fn outcome(self) -> Option<Outcome>
    {
        match self
        {
            Self::BelowDisplayMinimum { .. } => Some(Outcome::TooSmall),
            Self::ExceedsDisplayMaximum { .. } => Some(Outcome::TooLarge),
            Self::NoMemory | Self::AllocationTooSmall { .. } => Some(Outcome::InsufficientMemory),
            Self::Dma(_) => Some(Outcome::DeviceRefused),
            Self::Device(_) | Self::RestoreFailed { .. } => Some(Outcome::DeviceRefused),
            _ => None
        }
    }
}
impl fmt::Display for Refusal
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::BelowDisplayMinimum { width,
                                        height } =>
            {
                write!(f,
                       "requested {width}x{height} is below the minimum display size {}x{}",
                       Bounds::MINIMUM_SIDE,
                       Bounds::MINIMUM_SIDE)
            }
            Self::ExceedsDisplayMaximum { width,
                                          height,
                                          field,
                                          limit } =>
            {
                write!(f, "requested {width}x{height} exceeds display.{field} {limit}")
            }
            Self::AlreadyAttached =>
            {
                f.write_str("a framebuffer surface is already attached; System must release it")
            }
            Self::NotAttached => f.write_str("no framebuffer surface is attached"),
            Self::ModeChangePending =>
            {
                f.write_str("a mode change is prepared but not committed; presenting now would \
                             use a pitch the firmware no longer scans out")
            }
            Self::PreparePending =>
            {
                f.write_str("a mode change is already prepared and awaiting commit or abort")
            }
            Self::UnknownAllocation { allocation } =>
            {
                write!(f, "allocation {allocation} is not held for this operation")
            }
            Self::StaleGeneration { allocation,
                                    expected,
                                    observed } =>
            {
                write!(f,
                       "allocation {allocation} belongs to generation {expected}, not {observed}")
            }
            Self::Device(error) =>
            {
                write!(f, "mailbox framebuffer refused: {error}")
            }
            Self::NoMemory => f.write_str("the Device DMA service refused the framebuffer surface"),
            Self::Dma(status) =>
            {
                write!(f, "the Device DMA service returned status {status}")
            }
            Self::AllocationTooSmall { available,
                                       needed } =>
            {
                write!(f,
                       "the firmware allocated {available} bytes, less than the {needed} the mode \
                        needs")
            }
            Self::RestoreFailed { underlying } =>
            {
                write!(f,
                       "could not restore the previous display mode after a failed preparation: \
                        {underlying}")
            }
            Self::Clock => f.write_str("the system clock service refused a monotonic reading")
        }
    }
}
impl core::error::Error for Refusal
{
}
impl From<mac_videocore_fb::Error> for Refusal
{
    fn from(error: mac_videocore_fb::Error) -> Self
    {
        Self::Device(error)
    }
}

/// The drawing surface's bytes for `geometry`, taken from the firmware's pitch
/// rather than assuming a tightly packed buffer.
pub(crate) fn surface_bytes(frame: &Frame) -> u64
{
    u64::from(frame.pitch) * u64::from(frame.geometry.height)
}

/// The mode this driver asks the firmware for: 32-bit XRGB, channel order 1.
#[cfg(target_arch = "aarch64")]
pub(crate) const fn library_mode(geometry: mac_video_interface::Size) -> mac_videocore_fb::Mode
{
    mac_videocore_fb::Mode { width:       geometry.width,
                             height:      geometry.height,
                             depth:       scanout::DEPTH,
                             pixel_order: mac_videocore_fb::PixelOrder::Bgr }
}
