//! External timed-output driver: an SPI shift register fed by a leased DMA
//! channel.
//!
//! System selects this driver by its `WAVE` interface, never by file name, and
//! matches it to the SPI window the platform record declares. The class's reason
//! for existing is that no application thread can hold a sub-microsecond
//! deadline on a preemptive system, so this driver makes the *hardware* hold it:
//! a buffer computed by the caller is shifted out at a divided clock while a DMA
//! channel keeps the shift register supplied.
//!
//! Two claims, both exclusive, both leased rather than assumed: the SPI page from
//! the device inventory, and one channel from the DMA channel pool. The channel
//! is requested as "any", so a second serialiser takes a different one instead of
//! being refused the whole controller.
//!
//! The engine, the rate arithmetic and the refusal set are safe Rust and are
//! exercised on the host against a simulated serialiser. Register programming,
//! the channel lease and the cache maintenance live in one audited adapter
//! (`native`).
#![no_std]
#![deny(unsafe_code)]

use core::fmt;

pub mod engine;

#[cfg(target_arch = "aarch64")]
mod native;

pub use engine::{Abilities, Engine, Platform};
#[cfg(target_arch = "aarch64")]
pub use native::DRIVER;

/// Every way this driver refuses a request, each naming the numbers that caused
/// it. A missing capability is reported with what the serialiser *can* do, so a
/// caller can degrade rather than guess.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal
{
    /// The element width is not one this serialiser emits.
    ElementWidth
    {
        offered:   u32,
        requested: u32
    },
    /// The rate is outside what the divisor can reach.
    ElementRate
    {
        maximum:   u32,
        requested: u32
    },
    /// The buffer is larger than the class or the attachment allows.
    BufferTooLarge
    {
        capacity:  u32,
        requested: u32
    },
    /// The element count does not match the buffer that arrived.
    ElementCount
    {
        expected: usize,
        stated:   u32
    },
    EmptySubmission,
    /// A frame is still emitting. The caller must wait or cancel; the driver will
    /// not queue, because the inter-frame gap belongs to the caller.
    PreviousFrameInFlight,
    /// The previous frame has just finished; the same request will be accepted.
    Retry,
    NotAttached,
    AlreadyAttached,
    /// The page or the channel could not be claimed.
    ResourceUnavailable
    {
        what: &'static str
    },
    /// The hardware reported an error during a transfer.
    Hardware
    {
        status: u32
    },
    /// The device DMA service refused an allocation.
    Dma(i32),
    /// The clock the driver reads its monotonic time from is unavailable.
    Clock
}

impl fmt::Display for Refusal
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::ElementWidth { offered, requested } =>
            {
                write!(f, "element width {requested} bits is not offered; this serialiser emits \
                           up to {offered} bits")
            }
            Self::ElementRate { maximum, requested } =>
            {
                write!(f, "element rate {requested} Hz exceeds this serialiser's {maximum} Hz")
            }
            Self::BufferTooLarge { capacity, requested } =>
            {
                write!(f, "requested {requested} elements; the limit here is {capacity}")
            }
            Self::ElementCount { expected, stated } =>
            {
                write!(f, "the buffer holds {expected} elements but the submission states \
                           {stated}")
            }
            Self::EmptySubmission => write!(f, "a submission must carry at least one element"),
            Self::PreviousFrameInFlight =>
            {
                write!(f, "the previous frame is still emitting; wait for it or cancel it")
            }
            Self::Retry => write!(f, "the previous frame finished; submit again"),
            Self::NotAttached => write!(f, "nothing is attached"),
            Self::AlreadyAttached => write!(f, "this serialiser already holds an attachment"),
            Self::ResourceUnavailable { what } => write!(f, "{what} is unavailable"),
            Self::Hardware { status } => write!(f, "the serialiser reported status {status:#x}"),
            Self::Dma(status) => write!(f, "the device DMA service refused with status {status}"),
            Self::Clock => write!(f, "the monotonic clock is unavailable")
        }
    }
}
