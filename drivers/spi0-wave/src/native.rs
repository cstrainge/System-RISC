//! The one audited adapter, which now performs no register writes at all.
//!
//! **What it used to do.** It leased a DMA channel, allocated Device DMA storage,
//! filled a different static buffer, and set a bit it called "DMA enable". The
//! controller was never programmed: no transfer descriptor, no channel enable, no
//! pacing. And the bit was not the DMA enable — in this block's chip-select
//! register, bit 4 clears the transmit FIFO, so the driver emptied the FIFO and
//! reported itself configured. Its release then forgot the device claim it had
//! taken, leaving the page owned by a driver with no state pointing at it.
//!
//! **What it does now.** Nothing it cannot stand behind. Emitting needs the
//! transmit FIFO kept supplied, which is either a programmed DMA channel or a
//! verified answer to "when may the CPU write the FIFO". Neither exists here: no
//! source this project holds records that handshake's bit positions, and writing
//! DMA means writing descriptor, pacing and completion code that nothing in this
//! repository can exercise. So the adapter claims no channel, allocates no storage,
//! advertises no capability and refuses a submission by name.
//!
//! It cannot leak what it does not hold: the claim that leaked was the page it took
//! to program a transfer that was never going to run. `docs/TIMED-OUTPUT-CLASS.md`
//! records what would re-admit the driver.
#![forbid(unsafe_code)]

use mac_driver::{Client, Completion, Driver, Error, Request};
use mac_wave_interface::{self as wave, Attach, Granted, Progress, State, Submission};
use spinning_top::Spinlock;

use crate::{Abilities, Engine, Platform, Refusal};

include!(concat!(env!("OUT_DIR"), "/platform-spi.rs"));

/// A serialiser that provides the class and offers nothing in it.
struct Machine;

impl Machine
{
    const fn new() -> Self
    {
        Self
    }
}

impl Platform for Machine
{
    /// Zero capabilities, zero limits.
    ///
    /// The rate arithmetic elsewhere in this crate is real, but a rate nobody can
    /// shift a byte at is not a capability. Reporting `ELEMENT_8` here would put a
    /// serialiser on the board that every caller would plan around and none could
    /// use.
    fn abilities(&self) -> Abilities
    {
        Abilities { capabilities: 0,
                    maximum_hz:   0,
                    minimum_hz:   0,
                    element_bits: 0,
                    max_elements: 0 }
    }

    /// Refused. The engine reaches this only for a platform that offers the
    /// requested width, which this one never does; the refusal is here so the seam
    /// cannot silently accept a transfer.
    fn attach(&mut self, _attach: &Attach) -> Result<Granted, Refusal>
    {
        Err(Refusal::TransmitPathUnavailable)
    }

    fn submit(&mut self, _elements: &[u8], _submission: &Submission, _attached: &Granted)
              -> Result<(), Refusal>
    {
        Err(Refusal::TransmitPathUnavailable)
    }

    fn status(&mut self) -> Result<Progress, Refusal>
    {
        // Nothing was ever submitted, so the honest answer is that there is no
        // transfer to report on rather than a completion nobody earned.
        Ok(Progress { state:   State::Idle.code(),
                      emitted: 0,
                      reason:  0 })
    }

    fn cancel(&mut self) -> Result<Progress, Refusal>
    {
        Err(Refusal::NotAttached)
    }

    fn release(&mut self) -> Result<(), Refusal>
    {
        // Nothing is held, so there is nothing to give back. This is the whole
        // release-leak fix: the claim that leaked belonged to a transfer path that
        // does not exist.
        Ok(())
    }
}

static DEVICE: Spinlock<Option<(Machine, Engine)>> = Spinlock::new(None);

pub struct Spi0Wave;
pub static DRIVER: Spi0Wave = Spi0Wave;
impl Driver for Spi0Wave
{
    fn initialize(&self, _host: Client) -> mac_driver::Result<()>
    {
        // A driver for a block the record does not declare has nothing to do.
        if !PLATFORM_DECLARES_SPI
        {
            return Err(Error(mac_abi::NOT_SUPPORTED));
        }
        let mut slot = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
        if slot.is_some()
        {
            return Err(Error(mac_abi::SERVICE_BUSY));
        }
        // No service is resolved: this driver makes no request of the resident,
        // because it claims no page and leases no channel.
        *slot = Some((Machine::new(), Engine::new()));
        Ok(())
    }

    fn request(&self, request: Request<'_>, output: &mut [u8]) -> mac_driver::Result<Completion>
    {
        let mut slot = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
        let (machine, engine) = slot.as_mut().ok_or(Error(mac_abi::NOT_FOUND))?;
        // The wire reply is a status code, so the engine's named refusal cannot
        // travel with it; System sees the status, and a diagnostic build can read
        // the refusal from the engine.
        match request.operation
        {
            wave::ATTACH =>
            {
                let attach = Attach::decode(request.input).ok_or(Error(mac_abi::BAD_ARGUMENT))?;
                let granted = status(engine.attach(machine, attach))?;
                reply(output, &granted.encode())
            }
            wave::SUBMIT =>
            {
                if request.input.len() < Submission::BYTES
                {
                    return Err(Error(mac_abi::BAD_ARGUMENT));
                }
                let submission = Submission::decode(&request.input[..Submission::BYTES])
                    .ok_or(Error(mac_abi::BAD_ARGUMENT))?;
                status(engine.submit(machine, &request.input[Submission::BYTES..], submission))?;
                Ok(Completion::Complete(0))
            }
            wave::STATUS => reply(output, &status(engine.status(machine))?.encode()),
            wave::CANCEL => reply(output, &status(engine.cancel(machine))?.encode()),
            wave::RELEASE =>
            {
                status(engine.release(machine))?;
                Ok(Completion::Complete(0))
            }
            _ => Err(Error(mac_abi::NOT_SUPPORTED))
        }
    }

    fn quiesce(&self) -> mac_driver::Result<()>
    {
        let mut slot = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
        if let Some((machine, engine)) = slot.as_mut()
        {
            let _ = engine.release(machine);
        }
        *slot = None;
        Ok(())
    }
}

fn reply(output: &mut [u8], bytes: &[u8]) -> mac_driver::Result<Completion>
{
    output.get_mut(..bytes.len())
          .ok_or(Error(mac_abi::BAD_ARGUMENT))?
          .copy_from_slice(bytes);
    Ok(Completion::Complete(bytes.len()))
}

/// Translate a named refusal into the status the wire carries. The name stays in
/// the refusal, which is what a diagnostic build and the class contract report.
fn status<T>(result: Result<T, Refusal>) -> mac_driver::Result<T>
{
    result.map_err(|refusal| Error(refusal_code(refusal)))
}

/// The status code a named refusal maps to.
fn refusal_code(refusal: Refusal) -> u32
{
    match refusal
    {
        Refusal::NotAttached => mac_abi::NOT_FOUND,
        Refusal::TransmitPathUnavailable => mac_abi::NOT_SUPPORTED,
        Refusal::AlreadyAttached => mac_abi::SERVICE_BUSY,
        Refusal::Clock => mac_abi::IO_ERROR,
        _ => mac_abi::BAD_ARGUMENT
    }
}