//! The one audited machine adapter: the claimed entropy page, its registers, and
//! the request dispatch the entropy class defines.
//!
//! Everything above it is safe code. The order here follows
//! `docs/BCM2837-RNG.md`: claim the page, mask the line, enable the generator,
//! prove it by taking a word, and only then accept a request. Each step's reason
//! is recorded where the step is.
#![allow(unsafe_code)]

use core::ptr::{read_volatile, write_volatile};

use mac_driver::{Client, Completion, Driver, Error, Phase, Request, Result as DriverResult,
                 devices::DeviceService};
use mac_entropy_interface as entropy;
use spinning_top::Spinlock;

use crate::{Refusal, Registers, Rng};

include!(concat!(env!("OUT_DIR"), "/platform-rng.rs"));

/// The device's `compatible`, as the record's inventory key spells it. The key is
/// the one the resident matches a claimed window against, so it is written once
/// here and once in `module.json`, where the loader uses the same 16 bytes.
const RNG_KEY: [u8; 16] = *b"brcm,bcm2835-rng";

static DEVICE: Spinlock<Option<Machine>> = Spinlock::new(None);
pub struct Entropy;
pub static DRIVER: Entropy = Entropy;

/// Why the source is not answering, in the terms a caller can act on.
///
/// Kept as a value rather than a formatted string so that ERROR_TEXT reports the
/// refusal that actually happened, and so that a refusal cannot be turned into a
/// success by a later message.
#[derive(Clone, Copy)]
enum Cause
{
    None,
    Refused(Refusal),
    Driver(i32)
}

impl core::fmt::Display for Cause
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::None => write!(f, "the entropy source is answering"),
            Self::Refused(refusal) => write!(f, "{refusal}"),
            Self::Driver(status) => write!(f, "the driver refused with status {status}")
        }
    }
}

struct Machine
{
    service: DeviceService,
    claim:   u64,
    base:    usize,
    source:  Rng,
    cause:   Cause
}

impl Machine
{
    fn new(host: Client) -> DriverResult<Self>
    {
        Ok(Self { service: DeviceService::new(host)?,
                  claim: 0,
                  base: 0,
                  source: Rng::new(),
                  cause: Cause::None })
    }

    /// Claim the entropy page and establish a source inside it.
    ///
    /// Nothing is touched until the page is claimed, and a page that is not the one
    /// the record declares is refused rather than used: the record is the author of
    /// this machine, and a driver that accepted whatever it was handed would be
    /// reading registers it never had reason to believe were the ring oscillator.
    fn discover(&mut self) -> DriverResult<()>
    {
        if self.claim != 0
        {
            return Ok(());
        }
        if !PLATFORM_DECLARES_RNG
        {
            self.cause = Cause::Driver(mac_abi::NOT_SUPPORTED);
            return Err(Error(mac_abi::NOT_SUPPORTED));
        }
        let slots = self.service.count()?;
        for ordinal in 0..slots
        {
            let description = match self.service.probe(ordinal)
            {
                Ok(description) => description,
                Err(Error(mac_abi::NOT_FOUND)) => continue,
                Err(error) => return Err(error)
            };
            if description.bus.0 != *b"MMIO"
               || description.key != RNG_KEY
               || description.flags & mac_driver::devices::interface::MMIO == 0
            {
                continue;
            }
            // The record's window must sit inside the page the inventory claims.
            // That agreement is what makes the two record entries describe one
            // device rather than two.
            let page_end = description.registers
                                      .checked_add(description.register_bytes)
                                      .ok_or(Error(mac_abi::BAD_ABI))?;
            let window_end =
                (RNG_BASE as u64).checked_add(RNG_BYTES as u64).ok_or(Error(mac_abi::BAD_ABI))?;
            if (RNG_BASE as u64) < description.registers || window_end > page_end
            {
                self.cause = Cause::Driver(mac_abi::BAD_ABI);
                return Err(Error(mac_abi::BAD_ABI));
            }
            let claim = self.service.claim(ordinal)?;
            self.claim = claim.handle;
            if claim.registers != description.registers
               || claim.register_bytes != description.register_bytes
               || claim.key != description.key
            {
                self.cause = Cause::Driver(mac_abi::BAD_ABI);
                return Err(Error(mac_abi::BAD_ABI));
            }
            self.base = RNG_BASE;
            break;
        }
        if self.claim == 0
        {
            // The board does not carry the block, or its window is already held by
            // another claimant. Either way this driver has no source and says so;
            // it does not fall back to another device.
            self.cause = Cause::Driver(mac_abi::NOT_FOUND);
            return Err(Error(mac_abi::NOT_FOUND));
        }
        // Establish the source. A block that accepts the enable and counts nothing
        // is refused here rather than reported ready, which is the whole point of
        // proving it with a word.
        let mut registers = MachineRegisters { base: self.base };
        match self.source.start(&mut registers)
        {
            Ok(()) => Ok(()),
            Err(refusal) =>
            {
                self.release();
                self.cause = Cause::Refused(refusal);
                Err(Error(mac_abi::IO_ERROR))
            }
        }
    }

    fn release(&mut self)
    {
        if self.claim == 0
        {
            return;
        }
        let mut registers = MachineRegisters { base: self.base };
        self.source.stop(&mut registers);
        // A refused release keeps its claim, which is what the device service
        // requires: the handle stays valid so the resident can reclaim it later.
        if self.service.release(self.claim).is_ok()
        {
            self.claim = 0;
            self.base = 0;
        }
    }

    fn request(&mut self, request: Request<'_>, output: &mut [u8]) -> DriverResult<Completion>
    {
        if request.class != entropy::CLASS
           || !matches!(request.operation, entropy::READY | entropy::FILL | entropy::ERROR_TEXT)
        {
            return Err(Error(mac_abi::NOT_SUPPORTED));
        }
        if request.ticket == 0 || !request.input.is_empty()
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        if request.operation == entropy::ERROR_TEXT
        {
            return self.error_text(output);
        }
        if request.phase == Phase::Cancel
        {
            // Nothing is retained between calls: a fill either supplies bytes
            // before returning or refuses. Cancellation therefore has no
            // outstanding work to unwind, and says so rather than pretending it
            // stopped something.
            return Ok(Completion::Complete(0));
        }
        if (request.operation == entropy::READY && !output.is_empty())
           || (request.operation == entropy::FILL
               && (output.is_empty() || output.len() > entropy::MAX_TRANSFER))
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        if let Err(error) = self.discover()
        {
            return Err(error);
        }
        if request.operation == entropy::READY
        {
            // Completing only from the source's own proof, never from the claim.
            if !self.source.established()
            {
                self.cause = Cause::Driver(mac_abi::IO_ERROR);
                return Err(Error(mac_abi::IO_ERROR));
            }
            self.cause = Cause::None;
            return Ok(Completion::Complete(0));
        }
        let mut registers = MachineRegisters { base: self.base };
        match self.source.fill(&mut registers, output)
        {
            Ok(bytes) =>
            {
                self.cause = Cause::None;
                Ok(Completion::Complete(bytes))
            }
            Err(refusal) =>
            {
                // The contract's sharpest rule: a fill never completes with zero
                // bytes. The refusal carries why, and the count of polls behind it.
                self.cause = Cause::Refused(refusal);
                Err(Error(mac_abi::IO_ERROR))
            }
        }
    }

    fn error_text(&self, output: &mut [u8]) -> DriverResult<Completion>
    {
        struct Text
        {
            bytes:  [u8; entropy::MAX_ERROR_TEXT],
            length: usize
        }
        impl core::fmt::Write for Text
        {
            fn write_str(&mut self, text: &str) -> core::fmt::Result
            {
                let end = self.length.checked_add(text.len()).ok_or(core::fmt::Error)?;
                let destination = self.bytes.get_mut(self.length..end).ok_or(core::fmt::Error)?;
                destination.copy_from_slice(text.as_bytes());
                self.length = end;
                Ok(())
            }
        }
        let mut text = Text { bytes:  [0; entropy::MAX_ERROR_TEXT],
                              length: 0 };
        // No allocation: this is a bounded snapshot, which is what the class asks
        // for, and a heap use here would make a diagnostic depend on the allocator
        // it is meant to report on.
        if self.claim == 0
        {
            let _ = core::fmt::Write::write_str(&mut text, "no entropy page claimed");
        }
        else
        {
            use core::fmt::Write;
            let _ = write!(text,
                           "{}; words supplied={}",
                           self.cause,
                           self.source.supplied());
        }
        if output.len() < text.length || output.len() > entropy::MAX_ERROR_TEXT
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        output[..text.length].copy_from_slice(&text.bytes[..text.length]);
        Ok(Completion::Complete(text.length))
    }
}

/// The register seam over the claimed page.
///
/// The base is the record's window inside a page the resident granted for this
/// claim's lifetime, so every offset here is inside memory this driver owns and
/// no other module can reach.
struct MachineRegisters
{
    base: usize
}

impl Registers for MachineRegisters
{
    fn read(&mut self, offset: usize) -> u32
    {
        debug_assert!(offset < RNG_BYTES, "an entropy register offset is inside the claimed window");
        // SAFETY: `offset` is one of this block's registers and the page is claimed
        // and mapped Device memory for the claim's lifetime.
        unsafe { u32::from_le(read_volatile((self.base + offset) as *const u32)) }
    }

    fn write(&mut self, offset: usize, value: u32)
    {
        debug_assert!(offset < RNG_BYTES, "an entropy register offset is inside the claimed window");
        // SAFETY: as `read`; one register of the claimed page.
        unsafe { write_volatile((self.base + offset) as *mut u32, value.to_le()) };
    }
}

impl Driver for Entropy
{
    fn initialize(&self, host: Client) -> DriverResult<()>
    {
        let mut slot = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
        if slot.is_some()
        {
            return Err(Error(mac_abi::SERVICE_BUSY));
        }
        // Construction resolves the services only. The page is claimed and the
        // source established at the first request that needs it, so a machine that
        // declares no entropy block is refused at load with a reason instead of
        // failing startup for every module behind it.
        *slot = Some(Machine::new(host)?);
        Ok(())
    }

    fn request(&self, request: Request<'_>, output: &mut [u8]) -> DriverResult<Completion>
    {
        DEVICE.try_lock()
              .ok_or(Error(mac_abi::SERVICE_BUSY))?
              .as_mut()
              .ok_or(Error(mac_abi::NOT_FOUND))?
              .request(request, output)
    }

    fn quiesce(&self) -> DriverResult<()>
    {
        let mut slot = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
        if let Some(machine) = slot.as_mut()
        {
            machine.release();
        }
        *slot = None;
        Ok(())
    }
}
