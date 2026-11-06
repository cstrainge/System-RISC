//! The timed-output engine: what a caller is allowed to ask for, what the driver
//! answers, and the rules that are not about registers.
//!
//! Everything here is safe code and is exercised on the host against a
//! simulated serialiser. The machine adapter below it owns the registers, the
//! channel lease and the cache maintenance; nothing in this file knows what an
//! SPI block is.
use mac_wave_interface::{self as wave, Attach, Granted, Progress, State, Submission};

use crate::Refusal;

/// What a serialiser can do, in the terms the class uses. The driver fills this
/// in from its own hardware, so a capability is a claim the machine adapter
/// makes rather than a constant the engine assumes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Abilities
{
    /// Capability bits the driver offers, from `mac_wave_interface`.
    pub capabilities: u64,
    /// The fastest element rate the serialiser can produce, in hertz.
    pub maximum_hz:   u32,
    /// The slowest, in hertz. Zero means "as slow as the divisor allows", which
    /// every divider-based serialiser is.
    pub minimum_hz:   u32,
    /// The largest element the caller may emit, in bits.
    pub element_bits: u32,
    /// The largest buffer, in elements, one submission may carry.
    pub max_elements: u32
}

/// The seam the engine needs from the machine. One implementation per
/// serialiser; the engine never sees a register.
pub trait Platform
{
    fn abilities(&self) -> Abilities;
    /// Claim the serialiser and its feeder, and program the rate.
    fn attach(&mut self, attach: &Attach) -> Result<Granted, Refusal>;
    /// Hand the buffer to the hardware. The buffer is already encoded; the
    /// emitters count is the element count the engine validated.
    fn submit(&mut self, elements: &[u8], submission: &Submission, attached: &Granted)
              -> Result<(), Refusal>;
    /// Ask the hardware where the emission has reached. Never an elapsed-time
    /// estimate: the answer is the serialiser's own state.
    fn status(&mut self) -> Result<Progress, Refusal>;
    /// Stop the emission and leave the line at the idle level.
    fn cancel(&mut self) -> Result<Progress, Refusal>;
    /// Return the serialiser and the channel.
    fn release(&mut self) -> Result<(), Refusal>;
}

/// The engine's own state, which is the caller-visible contract.
pub struct Engine
{
    attached:  Option<(Attach, Granted)>,
    submitted: bool
}

impl Default for Engine
{
    fn default() -> Self
    {
        Self::new()
    }
}

impl Engine
{
    pub const fn new() -> Self
    {
        Self { attached:  None,
               submitted: false }
    }

    /// Attach, after checking the request against what this serialiser can do.
    ///
    /// Every refusal names the number that caused it: a rate outside the
    /// serialiser's range is reported as the range, not as "unsupported", and a
    /// width the driver does not offer is reported with the widths it does.
    pub fn attach(&mut self, platform: &mut impl Platform, attach: Attach)
                  -> Result<Granted, Refusal>
    {
        if self.attached.is_some()
        {
            return Err(Refusal::AlreadyAttached);
        }
        let abilities = platform.abilities();
        if attach.element_bits != 8 && attach.element_bits != 16 && attach.element_bits != 32
        {
            return Err(Refusal::ElementWidth { offered: abilities.element_bits,
                                               requested: attach.element_bits });
        }
        let bit = match attach.element_bits
        {
            8 => wave::ELEMENT_8,
            16 => wave::ELEMENT_16,
            _ => wave::ELEMENT_32
        };
        if abilities.capabilities & bit == 0
        {
            return Err(Refusal::ElementWidth { offered: abilities.element_bits,
                                               requested: attach.element_bits });
        }
        if attach.element_hz == 0 || attach.element_hz > abilities.maximum_hz
        {
            return Err(Refusal::ElementRate { maximum: abilities.maximum_hz,
                                              requested: attach.element_hz });
        }
        if attach.max_elements == 0 || attach.max_elements * (attach.element_bits / 8)
           > wave::MAX_TRANSFER as u32
        {
            return Err(Refusal::BufferTooLarge { capacity: wave::MAX_TRANSFER as u32,
                                                 requested: attach.max_elements });
        }
        let granted = platform.attach(&attach)?;
        self.attached = Some((attach, granted));
        self.submitted = false;
        Ok(granted)
    }

    /// Submit one buffer. One frame at a time: a second submission while the
    /// first is emitting is refused rather than queued, because the gap between
    /// frames is the caller's timing decision and not the driver's.
    pub fn submit(&mut self, platform: &mut impl Platform, elements: &[u8],
                  submission: Submission)
                  -> Result<(), Refusal>
    {
        let Some((attach, granted)) = self.attached
        else
        {
            return Err(Refusal::NotAttached);
        };
        if self.submitted
        {
            return match platform.status()?
            {
                Progress { state, .. }
                    if State::from_code(state) == Some(State::Emitting) =>
                {
                    Err(Refusal::PreviousFrameInFlight)
                }
                _ =>
                {
                    self.submitted = false;
                    Err(Refusal::Retry)
                }
            };
        }
        if submission.elements == 0
        {
            return Err(Refusal::EmptySubmission);
        }
        if submission.elements > attach.max_elements
        {
            return Err(Refusal::BufferTooLarge { capacity: attach.max_elements,
                                                 requested: submission.elements });
        }
        let width = (attach.element_bits / 8) as usize;
        if elements.len() != submission.elements as usize * width
        {
            return Err(Refusal::ElementCount { expected: elements.len() / width,
                                               stated:   submission.elements });
        }
        platform.submit(elements, &submission, &granted)?;
        self.submitted = true;
        Ok(())
    }

    pub fn status(&mut self, platform: &mut impl Platform) -> Result<Progress, Refusal>
    {
        if self.attached.is_none()
        {
            return Err(Refusal::NotAttached);
        }
        let progress = platform.status()?;
        if State::from_code(progress.state) != Some(State::Emitting)
        {
            self.submitted = false;
        }
        Ok(progress)
    }

    pub fn cancel(&mut self, platform: &mut impl Platform) -> Result<Progress, Refusal>
    {
        if self.attached.is_none()
        {
            return Err(Refusal::NotAttached);
        }
        let progress = platform.cancel()?;
        self.submitted = false;
        Ok(progress)
    }

    /// Release the claim. Refused rather than half-done if the hardware could not
    /// be stopped: a release that leaks a channel is worse than a refusal,
    /// because the leak is invisible to the next claimant.
    pub fn release(&mut self, platform: &mut impl Platform) -> Result<(), Refusal>
    {
        if self.attached.is_none()
        {
            return Err(Refusal::NotAttached);
        }
        platform.release()?;
        self.attached = None;
        self.submitted = false;
        Ok(())
    }

    pub fn attached(&self) -> Option<Granted>
    {
        self.attached.map(|(_, granted)| granted)
    }
}

/// The divisor that lands nearest `hz` for a serialiser clocked at `bus_hz`.
///
/// The result is reported honestly: a caller sees what it actually got, so a
/// quinance of the rate shows up as arithmetic rather than as a mystery pixel
/// colour. `None` when the clock cannot reach the rate at all.
pub const fn divisor_for(bus_hz: u32, hz: u32) -> Option<u32>
{
    if hz == 0 || bus_hz == 0 || hz > bus_hz
    {
        return None;
    }
    let divisor = bus_hz / hz;
    if divisor == 0 { Some(1) } else { Some(divisor) }
}

/// The rate a divisor actually produces.
pub const fn rate_for(bus_hz: u32, divisor: u32) -> u32
{
    if divisor == 0 || bus_hz == 0 { 0 } else { bus_hz / divisor }
}
