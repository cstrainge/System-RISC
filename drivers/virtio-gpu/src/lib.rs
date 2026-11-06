//! External Virtio Gpu video driver. The engine here is safe Rust: request
//! validation, the resize debounce and typed refusals. Device access lives in
//! one audited adapter (`machine`). System selects this driver by its VIDO
//! interface, never by file name.
#![no_std]
#![deny(unsafe_code)]
use mac_driver::{Completion, Error, Phase, Request, Result};
use mac_video_interface::{ABORT, APPLY, ATTACH, Attach, CLASS, COMMIT, Damage, Outcome, Poll,
                          Prepared, RELEASE, SERVICE, Size, Surface};

/// The host window must keep one size this long before it is reported. Window
/// dragging produces a configuration change per mouse movement; only the size
/// that survives this quiet period reaches System.
pub const QUIET_MICROSECONDS: u64 = 250_000;

pub trait Hardware
{
    /// Allocate the driver-owned framebuffer, create the initial resource and scan
    /// it out. Reports the buffer and the host's preferred size.
    fn attach(&mut self, attach: &Attach, generation: u64) -> Result<Surface>;
    /// `Some` when the host reported a display change since the last call.
    fn host_size(&mut self) -> Result<Option<Size>>;
    /// Present the changed rectangle of the current mode.
    fn present(&mut self, damage: Damage) -> Result<()>;
    /// Prepare a resource without changing scanout. COMMIT switches the device;
    /// RELEASE follows System's old CPU-pin retirement. Refusal retains custody.
    fn prepare(&mut self, size: Size, generation: u64) -> Result<Prepared>;
    fn commit(&mut self, allocation: u64, generation: u64) -> Result<Outcome>;
    fn abort(&mut self, allocation: u64, generation: u64) -> Result<()>;
    fn release(&mut self, allocation: u64, generation: u64) -> Result<()>;
    /// Nested VDEV work retains its original outer broker scope until ACK.
    fn pending(&self) -> bool;
    fn cancel(&mut self, generation: u64) -> Result<()>;
    fn microseconds(&mut self) -> Result<u64>;
}

/// Reports a settled size once; a new report needs a changed host size.
#[derive(Clone, Copy, Debug)]
pub struct Debounce
{
    applied:   Size,
    candidate: Option<Candidate>
}
#[derive(Clone, Copy, Debug)]
struct Candidate
{
    size:     Size,
    since:    u64,
    reported: bool
}
impl Debounce
{
    pub const fn new(applied: Size) -> Self
    {
        Self { applied,
               candidate: None }
    }

    pub fn observe(&mut self, host: Size, now: u64)
    {
        if host == self.applied || host.width == 0 || host.height == 0
        {
            self.candidate = None;
        }
        else if self.candidate.is_none_or(|candidate| candidate.size != host)
        {
            self.candidate = Some(Candidate { size:     host,
                                              since:    now,
                                              reported: false });
        }
    }

    pub fn settled(&mut self, now: u64) -> Option<Size>
    {
        let candidate = self.candidate.as_mut()?;
        if candidate.reported || now.saturating_sub(candidate.since) < QUIET_MICROSECONDS
        {
            return None;
        }
        candidate.reported = true;
        Some(candidate.size)
    }

    pub fn applied(&mut self, size: Size)
    {
        self.applied = size;
        if self.candidate.is_some_and(|candidate| candidate.size == size)
        {
            self.candidate = None;
        }
    }
}

#[derive(Clone, Copy)]
struct Limits
{
    min: Size,
    max: Size
}
pub struct Engine
{
    attached: Option<(Limits, Debounce)>,
    pending:  Option<(Size, u64, u64)>,
    failure:  Option<(u64, Error)>,
    initial:  Option<(Attach, Surface)>
}
impl Engine
{
    pub const fn new() -> Self
    {
        Self { attached: None,
               pending:  None,
               failure:  None,
               initial:  None }
    }

    pub fn request(&mut self, device: &mut impl Hardware, request: Request<'_>, output: &mut [u8])
                   -> Result<Completion>
    {
        if request.class != CLASS
        {
            return Err(Error(mac_abi::NOT_SUPPORTED));
        }
        if request.phase == Phase::Cancel
        {
            // A cancellation is terminal only after the SAME real nested
            // tickets and candidate finish. Refusal preserves the outer scope.
            if device.cancel(request.ticket).is_err()
            {
                return Ok(Completion::Pending);
            }
            if self.pending.is_some_and(|(_, _, generation)| generation == request.ticket)
            {
                self.pending = None;
            }
            if self.failure.is_some_and(|(generation, _)| generation == request.ticket)
            {
                self.failure = None;
            }
            return Ok(Completion::Complete(0));
        }
        if request.phase == Phase::Poll
        {
            let (generation, failure) = self.failure.ok_or(Error(mac_abi::BAD_ARGUMENT))?;
            if generation != request.ticket
            {
                return Err(Error(mac_abi::BAD_ARGUMENT));
            }
            if device.cancel(generation).is_err()
            {
                return Ok(Completion::Pending);
            }
            self.failure = None;
            return Err(failure);
        }
        if request.phase != Phase::Start
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        let result = match request.operation
        {
            ATTACH => self.attach(device, request.input, output, request.ticket),
            SERVICE => self.service(device, request.input, output),
            APPLY => self.prepare(device, request.input, output, request.ticket),
            COMMIT | ABORT | RELEASE =>
            {
                self.transition(device, request.operation, request.input, output)
            }
            _ => Err(Error(mac_abi::NOT_SUPPORTED))
        };
        match result
        {
            Err(failure) if device.pending() =>
            {
                self.failure = Some((request.ticket, failure));
                Ok(Completion::Pending)
            }
            completed => completed
        }
    }

    fn attach(&mut self, device: &mut impl Hardware, input: &[u8], output: &mut [u8],
              generation: u64)
              -> Result<Completion>
    {
        let attach = Attach::decode(input).ok_or(Error(mac_abi::BAD_ARGUMENT))?;
        if let Some((original, surface)) = self.initial
           && original == attach
        {
            if output.len() < Surface::BYTES
            {
                return Err(Error(mac_abi::BAD_ARGUMENT));
            }
            output[..Surface::BYTES].copy_from_slice(&surface.encode());
            return Ok(Completion::Complete(Surface::BYTES));
        }
        if self.attached.is_some()
        {
            return Err(Error(mac_abi::SERVICE_BUSY));
        }
        let min = Size { width:  attach.min_width,
                         height: attach.min_height };
        let max = Size { width:  attach.max_width,
                         height: attach.max_height };
        let current = Size { width:  attach.width,
                             height: attach.height };
        if generation == 0 || output.len() < Surface::BYTES || !within(current, min, max)
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        let surface = device.attach(&attach, generation)?;
        if bytes(current).is_none_or(|needed| needed > surface.capacity)
        {
            return Err(Error(mac_abi::BAD_ABI));
        }
        let now = device.microseconds()?;
        let mut debounce = Debounce::new(current);
        debounce.observe(surface.preferred, now);
        self.attached = Some((Limits { min,
                                       max },
                              debounce));
        self.initial = Some((attach, surface));
        output[..Surface::BYTES].copy_from_slice(&surface.encode());
        Ok(Completion::Complete(Surface::BYTES))
    }

    fn service(&mut self, device: &mut impl Hardware, input: &[u8], output: &mut [u8])
               -> Result<Completion>
    {
        if input.len() > Damage::BYTES * Damage::MAX_RECTS || input.len() % Damage::BYTES != 0
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        let (_, debounce) = self.attached.as_mut().ok_or(Error(mac_abi::NOT_FOUND))?;
        if output.len() < Poll::BYTES
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        let now = device.microseconds()?;
        if let Some(host) = device.host_size()?
        {
            debounce.observe(host, now);
        }
        for rect in input.chunks_exact(Damage::BYTES)
        {
            device.present(Damage::decode(rect).ok_or(Error(mac_abi::BAD_ARGUMENT))?)?;
        }
        let poll = Poll { settled: debounce.settled(now) };
        output[..Poll::BYTES].copy_from_slice(&poll.encode());
        Ok(Completion::Complete(Poll::BYTES))
    }

    fn prepare(&mut self, device: &mut impl Hardware, input: &[u8], output: &mut [u8],
               generation: u64)
               -> Result<Completion>
    {
        let size = Size::decode(input).ok_or(Error(mac_abi::BAD_ARGUMENT))?;
        let (limits, _) = self.attached.as_ref().ok_or(Error(mac_abi::NOT_FOUND))?;
        if generation == 0 || output.len() < Prepared::BYTES
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        if self.pending.is_some()
        {
            return Err(Error(mac_abi::SERVICE_BUSY));
        }
        let refused = if size.width < limits.min.width || size.height < limits.min.height
        {
            Some(Outcome::TooSmall)
        }
        else if size.width > limits.max.width || size.height > limits.max.height
        {
            Some(Outcome::TooLarge)
        }
        else
        {
            None
        };
        let prepared = if let Some(outcome) = refused
        {
            Prepared { outcome,
                       surface: None }
        }
        else
        {
            match device.prepare(size, generation)
            {
                Ok(prepared) => prepared,
                Err(Error(mac_abi::NO_MEMORY)) =>
                {
                    Prepared { outcome: Outcome::InsufficientMemory,
                               surface: None }
                }
                Err(error) => return Err(error)
            }
        };
        if prepared.outcome == Outcome::Applied
        {
            let surface = prepared.surface.ok_or(Error(mac_abi::BAD_ABI))?;
            self.pending = Some((size, surface.allocation, surface.generation));
        }
        output[..Prepared::BYTES].copy_from_slice(&prepared.encode());
        Ok(Completion::Complete(Prepared::BYTES))
    }

    fn transition(&mut self, device: &mut impl Hardware, operation: u32, input: &[u8],
                  output: &mut [u8])
                  -> Result<Completion>
    {
        if input.len() != 16
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        let allocation =
            u64::from_le_bytes(input[..8].try_into().map_err(|_| Error(mac_abi::BAD_ARGUMENT))?);
        let generation =
            u64::from_le_bytes(input[8..].try_into().map_err(|_| Error(mac_abi::BAD_ARGUMENT))?);
        if operation == RELEASE
        {
            device.release(allocation, generation)?;
            return Ok(Completion::Complete(0));
        }
        let Some((size, expected, expected_generation)) = self.pending
        else
        {
            // A retried ABORT can acknowledge only the hardware adapter's exact
            // previously completed token, never a different allocation/reuse.
            if operation == ABORT
            {
                device.abort(allocation, generation)?;
                return Ok(Completion::Complete(0));
            }
            return Err(Error(mac_abi::NOT_FOUND));
        };
        if expected != allocation || expected_generation != generation
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        if operation == ABORT
        {
            device.abort(allocation, generation)?;
            self.pending = None;
            return Ok(Completion::Complete(0));
        }
        if output.len() < Outcome::BYTES
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        let outcome = device.commit(allocation, generation)?;
        if outcome == Outcome::Applied
        {
            self.initial = None;
            self.attached.as_mut().ok_or(Error(mac_abi::NOT_FOUND))?.1.applied(size);
            self.pending = None;
        }
        output[..Outcome::BYTES].copy_from_slice(&outcome.code().to_le_bytes());
        Ok(Completion::Complete(Outcome::BYTES))
    }
}
impl Default for Engine
{
    fn default() -> Self
    {
        Self::new()
    }
}
fn within(size: Size, min: Size, max: Size) -> bool
{
    size.width >= min.width
    && size.height >= min.height
    && size.width <= max.width
    && size.height <= max.height
}
fn bytes(size: Size) -> Option<u64>
{
    u64::from(size.width).checked_mul(u64::from(size.height))?.checked_mul(4)
}

#[cfg(systemrisc)]
mod machine;
#[cfg(systemrisc)]
mod native
{
    use mac_driver::{Client, Completion, Driver, Error, Request, Result};
    use spinning_top::Spinlock;

    use super::{Engine, machine::Device};

    static DEVICE: Spinlock<Option<(Device, Engine)>> = Spinlock::new(None);
    pub struct VirtioGpu;
    pub static DRIVER: VirtioGpu = VirtioGpu;
    impl Driver for VirtioGpu
    {
        fn initialize(&self, host: Client) -> Result<()>
        {
            let mut slot = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
            if slot.is_some()
            {
                return Err(Error(mac_abi::SERVICE_BUSY));
            }
            // Construction acquires the claim only; hardware stays idle until
            // System attaches its framebuffer.
            *slot = Some((Device::new(host)?, Engine::new()));
            Ok(())
        }

        fn request(&self, request: Request<'_>, output: &mut [u8]) -> Result<Completion>
        {
            let mut slot = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
            let (device, engine) = slot.as_mut().ok_or(Error(mac_abi::NOT_FOUND))?;
            engine.request(device, request, output)
        }

        fn quiesce(&self) -> Result<()>
        {
            let mut slot = DEVICE.try_lock().ok_or(Error(mac_abi::SERVICE_BUSY))?;
            if let Some((device, _)) = slot.as_mut()
            {
                device.quiesce()?;
            }
            *slot = None;
            Ok(())
        }
    }
}
#[cfg(systemrisc)]
pub use native::DRIVER;
