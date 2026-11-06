//! BCM2837 ring-oscillator entropy, as a driver the System loads by interface.
//!
//! System selects this driver by its `ENTR` interface, never by file name, and
//! matches it to the entropy window the platform record declares. The whole
//! contract is `mac-entropy-interface`: READY establishes a source, FILL supplies
//! bytes, and only bytes actually supplied may complete a fill.
//!
//! **This block appears in no register document.** Every fact this driver relies
//! on is either labelled *documented* or *observed* in `docs/BCM2837-RNG.md`, and
//! the observed ones are inherited rather than measured by this project. The
//! driver is written to that document so a disagreement between the two is a
//! reviewable defect instead of folklore.
//!
//! Two consequences shape the code, and both are the point of the driver:
//!
//! * **A source that produces nothing must refuse, not fabricate.** A caller
//!   seeding a pool cannot distinguish four bytes of zero from four bytes of
//!   entropy, so returning zeros would be worse than failing. Every path that
//!   cannot supply a byte ends in a refusal that says so.
//! * **Availability is read before every word.** The count is the block's own
//!   statement of what it has; reading data it did not count reads whatever the
//!   block returns with an empty FIFO, which is a plausible-looking wrong answer.
//!
//! The decisions above live here as safe code over a [`Registers`] seam, so they
//! are exercised on the host against a simulated block. The claim, the mapping and
//! the volatile access live in the one audited adapter.

#![no_std]
#![deny(unsafe_code)]

use core::fmt;

/// Registers, as `docs/BCM2837-RNG.md` records them. All 32-bit, little-endian.
pub const CONTROL: usize = 0x00;
pub const STATUS: usize = 0x04;
pub const DATA: usize = 0x08;
/// One word past the extent the machine description states, and the reason this
/// platform declares a `0x14`-byte window rather than `0x10`.
pub const INTERRUPT_MASK: usize = 0x10;

/// Control bit 0: enable the generator. The block is off until this is set, and
/// a word read while it is clear carries no entropy.
pub const ENABLE: u32 = 1 << 0;
/// Interrupt-mask bit 0: mask the block's line. This driver polls, so an
/// unmasked line would be an interrupt nobody handles.
pub const MASKED: u32 = 1 << 0;

/// Words discarded after enabling, before any is handed to a caller.
///
/// A ring oscillator needs time to settle and no source states how long. This is
/// therefore a chosen number, not a measured one: it is here, named, so that a
/// reader can disagree with it deliberately. `docs/BCM2837-RNG.md` says the same
/// thing in the same words.
pub const WARMUP_WORDS: u32 = 16;

/// How many times a single word is waited for before the source is called silent.
///
/// A bound in polls rather than in time, because the cost of one poll is a
/// property of the busy-wait on this machine and not something this driver may
/// claim to know. It is a refusal threshold: reach it and the driver says the
/// block produced nothing.
pub const PATIENCE: u32 = 1 << 16;

/// The block's registers, as the adapter provides them.
///
/// One seam, three operations. Everything else in this crate is arithmetic over
/// values read through here, which is what makes the refusals below testable
/// without hardware.
pub trait Registers
{
    fn read(&mut self, offset: usize) -> u32;
    fn write(&mut self, offset: usize, value: u32);
}

/// The words the block says are ready, from the top byte of the status register.
pub fn available(status: u32) -> u32
{
    status >> 24
}

/// Every way this driver refuses, each naming what it observed.
///
/// A refusal is a result, not a diagnostic string: the caller decides what to do
/// with "this source produced nothing", and the number reported is the evidence it
/// was decided on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal
{
    /// The block never counted a word, so there is no source here. Reported with
    /// the polls spent, because "asked 65,536 times and got nothing" is a
    /// different finding from "asked once and the block was busy".
    SourceSilent
    {
        polls: u32
    },
    /// A fill was attempted before the source was established. FILL does not
    /// implicitly start the block: doing so would let a fill complete on a word
    /// the caller never saw proven.
    NotStarted,
    /// A fill produced no byte at all. The entropy contract treats zero bytes as
    /// failure rather than success, and so does this.
    EmptyFill,
    /// The block counted a word but the read produced nothing usable for a
    /// request that needed more. Kept separate from `SourceSilent` because the
    /// block did answer, so the fault is in this driver or the block's FIFO
    /// accounting, not in the absence of a source.
    ShortFill
    {
        supplied: usize,
        requested: usize
    }
}

impl fmt::Display for Refusal
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::SourceSilent { polls } =>
            {
                write!(f, "the entropy block counted no word in {polls} polls, so this machine has \
                           no working ring oscillator at the declared window")
            }
            Self::NotStarted => write!(f, "the entropy source is not established"),
            Self::EmptyFill =>
            {
                write!(f, "the entropy block supplied no byte; a fill must not complete with zero \
                           bytes")
            }
            Self::ShortFill { supplied, requested } =>
            {
                write!(f, "the entropy block supplied {supplied} of {requested} bytes before it \
                           stopped counting words")
            }
        }
    }
}

/// The source's own state across requests.
///
/// A `started` flag rather than a re-probe per request: READY must complete from a
/// genuine source completion, and re-reading the control register would only prove
/// the driver's own earlier write.
#[derive(Clone, Copy, Debug)]
pub struct Rng
{
    started:  bool,
    supplied: u64
}

impl Rng
{
    pub const fn new() -> Self
    {
        Self { started: false, supplied: 0 }
    }

    /// Whether a word has actually been obtained from the block.
    pub fn established(&self) -> bool
    {
        self.started
    }

    /// Bytes this source has supplied since it was started.
    pub fn supplied(&self) -> u64
    {
        self.supplied
    }

    /// Bring the source up, and prove it by obtaining a word.
    ///
    /// The proof matters: the entropy contract says READY "completes only after a
    /// genuine source completion, never from registration or a configured-device
    /// lookup alone". Enabling a register and reporting success would be exactly
    /// that forbidden answer, because a block whose oscillator is dead accepts the
    /// enable and then counts nothing.
    ///
    /// The mask is written before the enable, so the block cannot raise a line
    /// nothing is waiting for in the window between the two.
    pub fn start(&mut self, registers: &mut impl Registers) -> Result<(), Refusal>
    {
        registers.write(INTERRUPT_MASK, MASKED);
        registers.write(CONTROL, ENABLE);
        self.started = false;
        self.supplied = 0;

        // Discard the settling words. A word here is only taken when the block
        // counted it, so warm-up cannot itself invent entropy.
        let mut discarded = 0;
        let mut polls = 0;
        while discarded < WARMUP_WORDS
        {
            let Some(_) = self.take(registers, &mut polls)? else
            {
                return Err(Refusal::SourceSilent { polls });
            };
            discarded += 1;
        }
        self.started = true;
        Ok(())
    }

    /// Supply up to `output.len()` bytes, and report how many were supplied.
    ///
    /// A short fill is legal for the entropy contract and expected here: the
    /// block's FIFO holds a handful of words, so a request for kilobytes drains it
    /// as it refills. What is not legal is a fill of zero bytes, which the adapter
    /// turns into a refusal rather than reporting success.
    pub fn fill(&mut self, registers: &mut impl Registers, output: &mut [u8])
                -> Result<usize, Refusal>
    {
        if !self.started
        {
            return Err(Refusal::NotStarted);
        }
        let mut written = 0;
        let mut polls = 0;
        while written < output.len()
        {
            let Some(word) = self.take(registers, &mut polls)? else
            {
                break;
            };
            let bytes = word.to_le_bytes();
            let take = (output.len() - written).min(bytes.len());
            output[written..written + take].copy_from_slice(&bytes[..take]);
            written += take;
        }
        self.supplied += written as u64;
        if written == 0
        {
            return Err(Refusal::EmptyFill);
        }
        Ok(written)
    }

    /// Stop the generator. The claim itself is the adapter's to release.
    ///
    /// A block left running with no owner is a leak a later claimant cannot
    /// detect, so quiescence clears the enable rather than only dropping the
    /// mapping.
    pub fn stop(&mut self, registers: &mut impl Registers)
    {
        registers.write(CONTROL, 0);
        self.started = false;
    }

    /// One word, only if the block counted one.
    ///
    /// Returns `None` after [`PATIENCE`] consecutive empty counts, leaving the
    /// decision to refuse to the caller. `polls` accumulates across the whole
    /// operation, so a refusal reports the total asked rather than the last word's.
    /// The status is read again before every word rather than cached, because a
    /// count is a statement about the FIFO now, not about what it held when the
    /// driver last looked.
    fn take(&self, registers: &mut impl Registers, polls: &mut u32) -> Result<Option<u32>, Refusal>
    {
        let mut waited = 0;
        loop
        {
            *polls = polls.saturating_add(1);
            if available(registers.read(STATUS)) > 0
            {
                return Ok(Some(registers.read(DATA)));
            }
            waited += 1;
            if waited >= PATIENCE
            {
                return Ok(None);
            }
        }
    }
}

impl Default for Rng
{
    fn default() -> Self
    {
        Self::new()
    }
}

#[cfg(target_arch = "aarch64")]
mod native;

#[cfg(target_arch = "aarch64")]
pub use native::DRIVER;
