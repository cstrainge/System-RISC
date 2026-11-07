//! External timed-output driver: the SPI0 shift register, as the WAVE class.
//!
//! System selects this driver by its `WAVE` interface, never by file name, and
//! matches it to the SPI window the platform record declares.
//!
//! **This build provides the class and advertises no capability.** It used to
//! claim a leased DMA channel and a Device DMA allocation, and program neither: no
//! transfer descriptor, no channel enable. The bit it wrote as "DMA enable" is not
//! that at all — in the chip-select register at bit 4 it clears the transmit FIFO —
//! so the driver emptied the FIFO and reported itself configured. A capability that
//! is advertised and never exercised is worse than a missing one, because everything
//! above it plans around it.
//!
//! Emitting needs the transmit FIFO kept supplied, and that is one of two things
//! this project does not have: a programmed DMA channel, or a verified answer to
//! "when may the CPU write the FIFO". No source here records that handshake's bit
//! positions, so writing it would repeat the mistake above with different bits. The
//! adapter therefore holds nothing, offers nothing, and refuses a submission by
//! name. `docs/TIMED-OUTPUT-CLASS.md` records what would re-admit it.
//!
//! The engine, the rate arithmetic and the refusal set remain safe Rust and are
//! exercised on the host; the adapter is one audited file that currently performs no
//! register writes at all.
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
    /// This build cannot shift a single element out, and says so rather than
    /// accepting a submission it would drop.
    ///
    /// Emitting needs the transmit FIFO kept supplied. That is either a programmed
    /// DMA channel — which this driver does not have, because a channel it never
    /// programs is the defect this refusal replaces — or a verified answer to "when
    /// may the CPU write the FIFO", whose bit positions no source this project has
    /// records. A driver that accepted the submission and emitted nothing would put
    /// a serialiser on the board that no caller can use.
    TransmitPathUnavailable,
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
            Self::TransmitPathUnavailable =>
            {
                write!(f, "this build cannot shift elements out: no DMA channel is programmed and \
                           the transmit-FIFO handshake is not established, so nothing is emitted \
                           rather than accepting a frame that would never leave")
            }
            Self::ResourceUnavailable { what } => write!(f, "{what} is unavailable"),
            Self::Hardware { status } => write!(f, "the serialiser reported status {status:#x}"),
            Self::Dma(status) => write!(f, "the device DMA service refused with status {status}"),
            Self::Clock => write!(f, "the monotonic clock is unavailable")
        }
    }
}
