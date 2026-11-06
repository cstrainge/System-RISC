//! Device-neutral timed-output contract for external kernel drivers.
//!
//! A caller hands a serialiser a buffer and a timing; the hardware emits it
//! without a CPU instruction per element, and completion is reported by the
//! hardware. The class exists because no application thread can hold a
//! sub-microsecond deadline on a preemptive system: the composition that a
//! caller actually wants (a strip of LEDs, a framed audio stream) belongs in a
//! System extension, and this is the mechanism it builds on.
//!
//! The claim is over a serialiser **and** the DMA channel that feeds it, as one
//! exclusive pair. The two are exclusive for different reasons — a serialiser
//! cannot emit two waveforms, and a channel cannot move two buffers — so a
//! refusal always names which of them was unavailable.
//!
//! One submission at a time per attached serialiser. A caller either waits for
//! the frame to finish or is refused; queuing frames inside a driver would put
//! the inter-frame gap under the driver's control, which is exactly the timing
//! decision the caller owns.
//!
//! All integers are little-endian.
#![no_std]
#![forbid(unsafe_code)]

use mac_module::ClassId;

pub const CLASS: ClassId = ClassId(*b"WAVE");
pub const VERSION: u32 = 1;

/// Capability bits. The set is extensible: a caller tolerates and ignores bits
/// it does not know, so a board gaining a capability never forces every caller
/// to be rebuilt.
pub const ELEMENT_8: u64 = 1 << 0;
pub const ELEMENT_16: u64 = 1 << 1;
pub const ELEMENT_32: u64 = 1 << 2;
/// The serialiser can be fed by DMA, so no CPU instruction stands between two
/// elements. Without it a "timed output" is software timing with extra steps.
pub const PACED: u64 = 1 << 3;
/// The driver reports completion from the hardware: the last element has left
/// the serialiser, not merely reached its FIFO.
pub const HARDWARE_COMPLETION: u64 = 1 << 4;
/// The claimed resources can be released and attached again.
pub const REATTACH: u64 = 1 << 5;

pub const ATTACH: u32 = 1;
pub const SUBMIT: u32 = 2;
pub const STATUS: u32 = 3;
pub const CANCEL: u32 = 4;
pub const RELEASE: u32 = 5;

/// Submission flags.
/// The caller wants STATUS to report completion; without it the driver may
/// consider the frame free as soon as it has started.
pub const COMPLETION_REQUIRED: u32 = 1 << 0;
/// The line is left at the idle level afterwards, which is what latches an
/// addressable strip.
pub const HOLD_IDLE: u32 = 1 << 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State
{
    /// Nothing has been submitted since the last completion.
    Idle,
    /// The hardware is emitting.
    Emitting,
    /// The last submission finished; `STATUS` reports how many elements left.
    Complete,
    /// A cancellation stopped the emission at a driver-chosen boundary.
    Cancelled,
    /// The submission was refused or failed; `STATUS` carries the reason.
    Failed
}

impl State
{
    pub const fn code(self) -> u32
    {
        match self
        {
            Self::Idle => 0,
            Self::Emitting => 1,
            Self::Complete => 2,
            Self::Cancelled => 3,
            Self::Failed => 4
        }
    }

    pub const fn from_code(code: u32) -> Option<Self>
    {
        match code
        {
            0 => Some(Self::Idle),
            1 => Some(Self::Emitting),
            2 => Some(Self::Complete),
            3 => Some(Self::Cancelled),
            4 => Some(Self::Failed),
            _ => None
        }
    }
}

/// ATTACH input: what the caller needs the serialiser to produce.
///
/// `element_bits` is the width of one emitted element, which the caller chooses
/// against the capability bits it was offered. `element_hz` is how many elements
/// per second it wants; the driver converts that to a divisor against the
/// serialiser's own clock and reports what it actually achieved rather than
/// silently rounding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attach
{
    pub element_bits: u32,
    pub element_hz:   u32,
    /// The level the line holds between frames.
    pub idle_level:   u32,
    /// The largest buffer, in elements, the caller will submit.
    pub max_elements: u32
}
impl Attach
{
    pub const BYTES: usize = 16;

    pub fn encode(&self) -> [u8; Self::BYTES]
    {
        let mut bytes = [0; Self::BYTES];
        for (index, value) in [self.element_bits, self.element_hz, self.idle_level,
                               self.max_elements].into_iter()
                                                          .enumerate()
        {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Option<Self>
    {
        if bytes.len() != Self::BYTES
        {
            return None;
        }
        let word = |at: usize| -> Option<u32> {
            Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
        };
        Some(Self { element_bits: word(0)?,
                    element_hz:   word(4)?,
                    idle_level:   word(8)?,
                    max_elements: word(12)? })
    }
}

/// ATTACH reply: what was actually granted.
///
/// `divisor` is the serialiser's own programming value, reported so a caller can
/// see the quantisation it is living with instead of guessing it. `channel` is
/// the leased feeder, reported for the same reason: a second serialiser taking a
/// different channel is visible from outside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Granted
{
    pub element_hz: u32,
    pub divisor:    u32,
    pub channel:    u32,
    pub capabilities: u32
}
impl Granted
{
    pub const BYTES: usize = 16;

    pub fn encode(&self) -> [u8; Self::BYTES]
    {
        let mut bytes = [0; Self::BYTES];
        for (index, value) in [self.element_hz, self.divisor, self.channel,
                               self.capabilities].into_iter()
                                                               .enumerate()
        {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Option<Self>
    {
        if bytes.len() != Self::BYTES
        {
            return None;
        }
        let word = |at: usize| -> Option<u32> {
            Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
        };
        Some(Self { element_hz:   word(0)?,
                    divisor:      word(4)?,
                    channel:      word(8)?,
                    capabilities: word(12)? })
    }
}

/// SUBMIT input header. The buffer follows it in the same request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Submission
{
    /// Elements in the buffer that follows.
    pub elements: u32,
    pub flags:    u32,
    /// Requested idle time after the last element, in microseconds. Only a
    /// minimum can be promised: between frames the system is in charge, so a
    /// caller needing a maximum must not use this class for it.
    pub gap_microseconds: u32
}
impl Submission
{
    pub const BYTES: usize = 12;

    pub fn encode(&self) -> [u8; Self::BYTES]
    {
        let mut bytes = [0; Self::BYTES];
        for (index, value) in [self.elements, self.flags, self.gap_microseconds].into_iter()
                                                                                  .enumerate()
        {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Option<Self>
    {
        if bytes.len() != Self::BYTES
        {
            return None;
        }
        let word = |at: usize| -> Option<u32> {
            Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
        };
        Some(Self { elements:         word(0)?,
                    flags:            word(4)?,
                    gap_microseconds: word(8)? })
    }
}

/// STATUS reply. `state` is a [`State`] code; `emitted` counts elements that
/// left the serialiser, which for a cancelled frame is where it stopped rather
/// than what was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress
{
    pub state:   u32,
    pub emitted: u32,
    /// A typed failure code the driver defines; zero when there is none.
    pub reason:  u32
}
impl Progress
{
    pub const BYTES: usize = 12;

    pub fn encode(&self) -> [u8; Self::BYTES]
    {
        let mut bytes = [0; Self::BYTES];
        for (index, value) in [self.state, self.emitted, self.reason].into_iter().enumerate()
        {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Option<Self>
    {
        if bytes.len() != Self::BYTES
        {
            return None;
        }
        let word = |at: usize| -> Option<u32> {
            Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
        };
        Some(Self { state:   word(0)?,
                    emitted: word(4)?,
                    reason:  word(8)? })
    }
}

/// The largest buffer one submission may carry, in bytes of payload.
pub const MAX_TRANSFER: usize = 2048;
/// The largest request: the header plus a full buffer.
pub const MAX_REQUEST: usize = Submission::BYTES + MAX_TRANSFER;
pub const MAX_REPLY: usize = Granted::BYTES;
