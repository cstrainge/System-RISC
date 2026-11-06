//! The one audited machine adapter: the SPI0 register window, the leased DMA
//! channel, the buffer the feeder reads and the cache maintenance that makes the
//! CPU's stores visible to it.
//!
//! Everything above it is safe code. The sequence here follows
//! `docs/BCM2837-SPI0-DMA.md` step by step, and each step's reason is recorded
//! where the step is.
#![allow(unsafe_code)]

use core::ptr::{read_volatile, write_volatile};

use mac_driver::{Client, Completion, Error, Phase, Result as DriverResult, devices::DeviceService};
use mac_pool_interface as pool;
use mac_wave_interface::{self as wave, Attach, Granted, Progress, State, Submission};
use spinning_top::Spinlock;

use crate::{Abilities, Engine, Platform, Refusal,
            engine::{divisor_for, rate_for}};

include!(concat!(env!("OUT_DIR"), "/platform-spi.rs"));

/// The `brcm,spi0` inventory key, the same value `module.json` matches on.
const SPI_KEY: [u8; 16] = *b"brcm,spi0\0\0\0\0\0\0\0";
/// The core clock the divider is computed against. The published register
/// document's nominal 150 MHz is not this board's rate; this is the board's own
/// figure, and the divisor arithmetic is only as good as it.
const CORE_HZ: u32 = 250_000_000;

// Register offsets, from the window base the record declares.
const CS: usize = 0x00;
const CLOCK: usize = 0x08;
const DLEN: usize = 0x0C;
const SPI_STATUS: usize = 0x14;

/// Chip-select field: transfer active. Set to start an emission, cleared to end
/// it; the block shifts only while it is set.
const CS_TA: u32 = 1 << 7;
/// DMA enable for the transmit FIFO. Without it the engine cannot service the
/// block and the buffer never moves.
const CS_DMAEN: u32 = 1 << 4;
/// Clear both FIFOs. Only ever set with no chip select asserted: clearing the
/// transmit FIFO with a select asserted can de-assert it for a fraction of a
/// clock period, which is one of the published document's recorded errors.
const CS_CLEAR: u32 = 3 << 4;
/// The block's own busy bit in the status register: a character is still being
/// shifted out.
const STATUS_BUSY: u32 = 1 << 7;

/// Deferred allocation deadline. The resident services the request from its own
/// loop; a service that never answers is a defect, not a wait to extend.
const ALLOCATION_TIMEOUT_MICROSECONDS: u64 = 1_000_000;
/// How long an emission may take before it is reported as failed. A frame the
/// caller submitted is bounded by the class's buffer limit, so a block still busy
/// after this is a stuck block rather than a slow one.
const TRANSFER_TIMEOUT_MICROSECONDS: u64 = 2_000_000;

/// The buffer the feeder reads. Word-aligned because a DMA control block moves
/// words, and static because the feeder reads it after the request returns.
#[repr(align(16))]
struct Storage([u32; wave::MAX_TRANSFER / 4]);
static mut BUFFER: Storage = Storage([0; wave::MAX_TRANSFER / 4]);

/// Clean `bytes` at `address` to the point of coherency, then wait for it.
///
/// The feeder reads memory, not cache: stores still in a dirty line are
/// invisible to it, so a waveform submitted without this emits whatever was in
/// memory before the buffer was filled — silently, because the transfer succeeds.
unsafe fn clean(address: usize, bytes: usize)
{
    unsafe {
        let ctr: u64;
        core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nostack, nomem));
        let line = 4usize << ((ctr >> 16) & 0xf);
        let end = address.saturating_add(bytes);
        let mut at = address & !(line - 1);
        while at < end
        {
            core::arch::asm!("dc cvac, {}", in(reg) at, options(nostack));
            at += line;
        }
        core::arch::asm!("dsb sy", options(nostack));
    }
}

/// The resident channel arbiter, as this driver sees it.
///
/// Separate from the device service because it answers a different question: the
/// device service grants a page, and this grants one resource inside a page that
/// another owner holds. The two are not interchangeable and the driver needs both.
struct Pool
{
    provider: mac_driver::Provider
}
impl Pool
{
    fn new(host: Client) -> DriverResult<Self>
    {
        Ok(Self { provider: host.resolve(pool::CLASS, pool::VERSION)? })
    }

    /// Lease one resource, preferring any free one: a serialiser that demanded a
    /// particular channel would exclude every other serialiser on the board.
    fn lease(&self, key: [u8; 16], resource: u32) -> DriverResult<pool::Lease>
    {
        let input = pool::Request_ { key, requirement: resource }.encode();
        let mut output = [0; pool::MAX_REPLY];
        let length = self.provider.synchronous(pool::LEASE, &input[..pool::Request_::BYTES],
                                               &mut output)?;
        pool::Lease::decode(&output[..length]).ok_or(Error(mac_abi::BAD_ABI))
    }

    fn release(&self, lease: pool::Lease) -> DriverResult<()>
    {
        let input = lease.encode();
        self.provider.synchronous(pool::RELEASE, &input[..pool::Lease::BYTES], &mut [])?;
        Ok(())
    }
}

/// Where a submission has reached, as the block reports it. Kept beside the
/// claim because it outlives one request.
#[derive(Clone, Copy, Default)]
struct Emission
{
    submitted: bool
}

struct Machine
{
    host:    Client,
    service: DeviceService,
    pool:    Option<Pool>,
    claim:   u64,
    channel: u64,
    resource: u32,
    base:    usize,
    /// The bus address of the transmit buffer and its length, from the Device DMA
    /// service. A CPU pointer here is the classic failure: the transfer succeeds
    /// and the bytes are wrong.
    buffer:  u64,
    buffer_bytes: u64,
    granted: Option<Granted>,
    emission: Emission
}
impl Machine
{
    fn new(host: Client) -> DriverResult<Self>
    {
        Ok(Self { host,
                  service: DeviceService::new(host)?,
                  pool: Pool::new(host).ok(),
                  claim: 0,
                  channel: 0,
                  resource: 0,
                  base: 0,
                  buffer: 0,
                  buffer_bytes: 0,
                  granted: None,
                  emission: Emission::default() })
    }

    /// Claim the SPI page, lease a feeder and map the transmit buffer.
    fn discover(&mut self) -> DriverResult<()>
    {
        if self.claim != 0
        {
            return Ok(());
        }
        if !PLATFORM_DECLARES_SPI || !POOL_DECLARED
        {
            // No SPI window, or no channel pool to feed it from. Either way there
            // is nothing this driver may touch, so it declines rather than
            // half-initialising.
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
               || description.key != SPI_KEY
               || description.flags & mac_driver::devices::interface::MMIO == 0
            {
                continue;
            }
            // The record's window must sit inside the page the inventory claims:
            // that agreement is what makes the two record entries describe one
            // device rather than two.
            let page_end = description.registers
                                      .checked_add(description.register_bytes)
                                      .ok_or(Error(mac_abi::BAD_ABI))?;
            let window_end = (SPI_BASE as u64).checked_add(SPI_BYTES as u64)
                                              .ok_or(Error(mac_abi::BAD_ABI))?;
            if (SPI_BASE as u64) < description.registers || window_end > page_end
            {
                return Err(Error(mac_abi::BAD_ABI));
            }
            let claim = self.service.claim(ordinal)?;
            self.claim = claim.handle;
            if claim.registers != description.registers
               || claim.register_bytes != description.register_bytes
               || claim.key != description.key
            {
                return Err(Error(mac_abi::BAD_ABI));
            }
            self.base = SPI_BASE;
            break;
        }
        if self.claim == 0
        {
            return Err(Error(mac_abi::NOT_FOUND));
        }
        let pool = self.pool.as_ref().ok_or(Error(mac_abi::NOT_SUPPORTED))?;
        let lease = pool.lease(POOL_KEY, pool::Request_::ANY_RESOURCE)?;
        self.channel = lease.handle;
        self.resource = lease.resource;
        // The feeder moves bus addresses, so the buffer is Device DMA storage.
        let allocation = self.allocate(wave::MAX_TRANSFER as u64)?;
        self.buffer = allocation.device_address;
        self.buffer_bytes = allocation.length;
        Ok(())
    }

    fn allocate(&mut self, bytes: u64) -> DriverResult<mac_driver::devices::interface::Allocation>
    {
        let mut ticket = 0;
        let deadline = self.microseconds()?
                           .saturating_add(ALLOCATION_TIMEOUT_MICROSECONDS);
        loop
        {
            let phase = if ticket == 0 { Phase::Start } else { Phase::Poll };
            let (completion, buffer) = self.service.allocate(self.claim, bytes, &mut ticket, phase)?;
            match (completion, buffer)
            {
                (Completion::Complete(_), Some(buffer)) => return Ok(buffer),
                (Completion::Pending, _) if self.microseconds()? < deadline => (),
                _ =>
                {
                    if ticket != 0
                    {
                        let _ =
                            self.service.allocate(self.claim, bytes, &mut ticket, Phase::Cancel);
                    }
                    return Err(Error(mac_abi::NO_MEMORY));
                }
            }
        }
    }

    fn read(&self, offset: usize) -> u32
    {
        // SAFETY: `offset` is one of this block's registers and the page is
        // claimed and mapped Device memory for the claim's lifetime.
        unsafe { u32::from_le(read_volatile((self.base + offset) as *const u32)) }
    }

    fn write(&self, offset: usize, value: u32)
    {
        // SAFETY: as `read`; one register of the claimed page.
        unsafe { write_volatile((self.base + offset) as *mut u32, value.to_le()) };
    }

    fn microseconds(&mut self) -> DriverResult<u64>
    {
        self.host.monotonic_microseconds()
    }

    /// Wait for the block to stop shifting, then stop it.
    ///
    /// The feeder finishing means the last word reached the FIFO; the busy bit
    /// clearing means it left the shift register. Completion is the second, and a
    /// driver that stopped at the first would drop the last word of every frame.
    fn finish(&mut self) -> DriverResult<Progress>
    {
        let deadline = self.microseconds()?
                           .saturating_add(TRANSFER_TIMEOUT_MICROSECONDS);
        loop
        {
            let status = self.read(SPI_STATUS);
            if status & STATUS_BUSY == 0
            {
                self.write(CS, CS_DMAEN);
                self.emission.submitted = false;
                return Ok(Progress { state:   State::Complete.code(),
                                     emitted: 0,
                                     reason:  0 });
            }
            if self.microseconds()? >= deadline
            {
                self.write(CS, CS_DMAEN);
                self.emission.submitted = false;
                return Ok(Progress { state:   State::Failed.code(),
                                     emitted: 0,
                                     reason:  status });
            }
        }
    }
}
impl Platform for Machine
{
    fn abilities(&self) -> Abilities
    {
        Abilities { capabilities: wave::ELEMENT_8
                                  | wave::ELEMENT_16
                                  | wave::ELEMENT_32
                                  | wave::PACED
                                  | wave::HARDWARE_COMPLETION
                                  | wave::REATTACH,
                    maximum_hz:   CORE_HZ,
                    minimum_hz:   1,
                    element_bits: 32,
                    max_elements: (wave::MAX_TRANSFER / 4) as u32 }
    }

    fn attach(&mut self, attach: &Attach) -> Result<Granted, Refusal>
    {
        self.discover().map_err(|error| Refusal::Dma(error.0))?;
        let divisor = divisor_for(CORE_HZ, attach.element_hz)
                          .ok_or(Refusal::ElementRate { maximum: CORE_HZ,
                                                        requested: attach.element_hz })?;
        let granted = Granted { element_hz:   rate_for(CORE_HZ, divisor),
                                divisor,
                                channel:      self.resource,
                                capabilities: self.abilities().capabilities as u32 };
        // Configure, but do not start: the transfer-active bit is set after the
        // feeder is running, and the other order leaves the block waiting on a
        // FIFO that nothing fills.
        self.write(CS, 0);
        self.write(CS, CS_CLEAR | CS_DMAEN);
        self.write(CLOCK, divisor);
        self.granted = Some(granted);
        Ok(granted)
    }

    fn submit(&mut self, elements: &[u8], _submission: &Submission, _attached: &Granted)
              -> Result<(), Refusal>
    {
        if elements.len() as u64 > self.buffer_bytes
        {
            return Err(Refusal::BufferTooLarge { capacity: self.buffer_bytes as u32,
                                                 requested: elements.len() as u32 });
        }
        // SAFETY: the buffer is this module's own static, the claim is exclusive
        // and no other writer exists; the copy is bounded by the storage the
        // class allows.
        unsafe {
            core::ptr::copy_nonoverlapping(elements.as_ptr(),
                                           core::ptr::addr_of_mut!(BUFFER.0) as *mut u8,
                                           elements.len())
        };
        // The feeder reads memory, not cache. Clean before it is told to start.
        // SAFETY: the range lies inside the module's own static buffer.
        unsafe { clean(core::ptr::addr_of_mut!(BUFFER.0) as usize, elements.len()) };
        // The data-length register counts BYTES. Writing a word count emits a
        // quarter of the frame, which on a strip is a truncated frame rather than
        // an error.
        self.write(DLEN, elements.len() as u32);
        self.write(CS, CS_DMAEN | CS_TA);
        self.emission.submitted = true;
        Ok(())
    }

    fn status(&mut self) -> Result<Progress, Refusal>
    {
        if self.granted.is_none()
        {
            return Err(Refusal::NotAttached);
        }
        if self.read(SPI_STATUS) & STATUS_BUSY != 0
        {
            return Ok(Progress { state:   State::Emitting.code(),
                                 emitted: 0,
                                 reason:  0 });
        }
        self.finish().map_err(|error| Refusal::Dma(error.0))
    }

    fn cancel(&mut self) -> Result<Progress, Refusal>
    {
        if self.granted.is_none()
        {
            return Err(Refusal::NotAttached);
        }
        // Stop at a boundary the block chooses and leave the line idle. The
        // caller is told this is a cancellation, not that the last element was
        // the last of the frame.
        self.write(CS, CS_DMAEN);
        self.write(CS, CS_CLEAR);
        self.emission.submitted = false;
        Ok(Progress { state:   State::Cancelled.code(),
                      emitted: 0,
                      reason:  0 })
    }

    fn release(&mut self) -> Result<(), Refusal>
    {
        self.write(CS, 0);
        self.write(CS, CS_CLEAR);
        self.granted = None;
        self.emission.submitted = false;
        if self.channel != 0
        {
            if let Some(pool) = &self.pool
            {
                let lease = pool::Lease { handle: self.channel, resource: self.resource };
                let _ = pool.release(lease);
            }
            self.channel = 0;
        }
        Ok(())
    }
}

static DEVICE: Spinlock<Option<(Machine, Engine)>> = Spinlock::new(None);

pub struct Spi0Wave;
pub static DRIVER: Spi0Wave = Spi0Wave;
impl mac_driver::Driver for Spi0Wave
{
    fn initialize(&self, host: Client) -> DriverResult<()>
    {
        let mut slot = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
        if slot.is_some()
        {
            return Err(Error(mac_abi::SERVICE_BUSY));
        }
        // Construction resolves the services only. The page and the channel are
        // claimed when a caller attaches, so a board with no caller holds no
        // hardware.
        *slot = Some((Machine::new(host)?, Engine::new()));
        Ok(())
    }

    fn request(&self, request: mac_driver::Request<'_>, output: &mut [u8])
               -> DriverResult<Completion>
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

    fn quiesce(&self) -> DriverResult<()>
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

fn reply(output: &mut [u8], bytes: &[u8]) -> DriverResult<Completion>
{
    output.get_mut(..bytes.len())
          .ok_or(Error(mac_abi::BAD_ARGUMENT))?
          .copy_from_slice(bytes);
    Ok(Completion::Complete(bytes.len()))
}

/// Translate a named refusal into the status the wire carries. The name stays in
/// the refusal, which is what a diagnostic build and the class contract report.
fn status<T>(result: Result<T, Refusal>) -> DriverResult<T>
{
    result.map_err(|refusal| match refusal
    {
        Refusal::NotAttached | Refusal::PreviousFrameInFlight | Refusal::Retry =>
        {
            Error(mac_abi::BAD_ARGUMENT)
        }
        Refusal::AlreadyAttached | Refusal::ResourceUnavailable { .. } =>
        {
            Error(mac_abi::SERVICE_BUSY)
        }
        Refusal::Dma(code) => Error(code),
        Refusal::Clock => Error(mac_abi::NOT_SUPPORTED),
        Refusal::Hardware { .. } => Error(mac_abi::BAD_ABI),
        Refusal::ElementWidth { .. }
        | Refusal::ElementRate { .. }
        | Refusal::BufferTooLarge { .. }
        | Refusal::ElementCount { .. }
        | Refusal::EmptySubmission => Error(mac_abi::BAD_ARGUMENT)
    })
}
