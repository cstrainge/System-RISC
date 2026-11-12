//! The device-neutral VIDO request state machine, driven over [`Hardware`].
//!
//! It validates the wire contract, keeps the applied mode and the one prepared
//! candidate, and turns this driver's named refusals into either a typed
//! [`Prepared`] outcome or a status, so System can tell "that mode is too large"
//! from "the device failed". Every refusal is also retained in
//! [`Engine::last_refusal`] for the adapter to log, because the wire carries a
//! status code and not a message.
use mac_driver::{Completion, Error, Phase, Request, Result};
use mac_video_interface::{ABORT, APPLY, ATTACH, Attach, CLASS, COMMIT, Damage, Outcome, Poll,
                          Prepared, RELEASE, SERVICE, Size, Surface};

use crate::{Refusal, device::Hardware};

#[derive(Clone, Copy)]
struct Limits
{
    min: Size,
    max: Size
}

pub struct Engine
{
    attached:     Option<(Limits, Size)>,
    pending:      Option<(Size, u64, u64)>,
    failure:      Option<(u64, Refusal)>,
    initial:      Option<(Attach, Surface)>,
    last_refusal: Option<Refusal>
}
impl Engine
{
    pub const fn new() -> Self
    {
        Self { attached:     None,
               pending:      None,
               failure:      None,
               initial:      None,
               last_refusal: None }
    }

    /// The named cause of the most recent refusal, for the adapter's log. The
    /// wire reply is a status code, so this is where the message survives.
    pub fn last_refusal(&self) -> Option<Refusal>
    {
        self.last_refusal
    }

    pub fn take_refusal(&mut self) -> Option<Refusal>
    {
        self.last_refusal.take()
    }

    fn refuse(&mut self, refusal: Refusal) -> Error
    {
        self.last_refusal = Some(refusal);
        Error(refusal.status())
    }

    pub fn request(&mut self, device: &mut impl Hardware, request: Request<'_>, output: &mut [u8])
                   -> Result<Completion>
    {
        self.last_refusal = None;
        if request.class != CLASS
        {
            return Err(Error(mac_abi::NOT_SUPPORTED));
        }
        if request.phase == Phase::Cancel
        {
            // A cancellation is terminal only after the same nested ticket and
            // candidate finish. Refusal preserves the outer scope.
            if let Err(refusal) = device.cancel(request.ticket)
            {
                self.last_refusal = Some(refusal);
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
            self.last_refusal = Some(failure);
            return Err(Error(failure.status()));
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
                let refusal = self.last_refusal.unwrap_or(Refusal::Dma(failure.0));
                self.failure = Some((request.ticket, refusal));
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
        let surface = device.attach(&attach, generation).map_err(|refusal| self.refuse(refusal))?;
        if bytes(current).is_none_or(|needed| needed > surface.capacity)
        {
            return Err(Error(mac_abi::BAD_ABI));
        }
        self.attached = Some((Limits { min,
                                       max },
                              current));
        self.initial = Some((attach, surface));
        output[..Surface::BYTES].copy_from_slice(&surface.encode());
        Ok(Completion::Complete(Surface::BYTES))
    }

    fn service(&mut self, device: &mut impl Hardware, input: &[u8], output: &mut [u8])
               -> Result<Completion>
    {
        // SERVICE carries a list of damaged rectangles, none or as many as the
        // contract's maximum, not exactly one. System sends whatever its dirty
        // tracking collected since the last call - nothing on an idle tick,
        // several when more than one window changed - and has already forgotten
        // them by the time this returns, so a refusal here is a lost repaint.
        if input.len() > Damage::BYTES * Damage::MAX_RECTS || input.len() % Damage::BYTES != 0
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        let (_, applied) = self.attached.ok_or(Error(mac_abi::NOT_FOUND))?;
        if output.len() < Poll::BYTES
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        for rect in input.chunks_exact(Damage::BYTES)
        {
            let damage = Damage::decode(rect).ok_or(Error(mac_abi::BAD_ARGUMENT))?;
            if damage.width != 0 && damage.height != 0
            {
                device.present(damage).map_err(|refusal| self.refuse(refusal))?;
            }
        }
        // This driver's firmware never reports a mode it chose, so the only
        // settled size is one the driver itself has not yet adopted. Reporting
        // it once is enough: System applies it and asks again.
        let settled = match device.host_size().map_err(|refusal| self.refuse(refusal))?
        {
            Some(size) if size != applied =>
            {
                self.attached.as_mut().ok_or(Error(mac_abi::NOT_FOUND))?.1 = size;
                Some(size)
            }
            _ => None
        };
        output[..Poll::BYTES].copy_from_slice(&Poll { settled }.encode());
        Ok(Completion::Complete(Poll::BYTES))
    }

    fn prepare(&mut self, device: &mut impl Hardware, input: &[u8], output: &mut [u8],
               generation: u64)
               -> Result<Completion>
    {
        let size = Size::decode(input).ok_or(Error(mac_abi::BAD_ARGUMENT))?;
        let (limits, _) = self.attached.ok_or(Error(mac_abi::NOT_FOUND))?;
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
                Err(refusal) =>
                {
                    // A mode the device will not accept is a typed outcome, not
                    // a transport failure; the named cause is kept for the log.
                    self.last_refusal = Some(refusal);
                    match refusal.outcome()
                    {
                        Some(outcome) =>
                        {
                            Prepared { outcome,
                                       surface: None }
                        }
                        None => return Err(Error(refusal.status()))
                    }
                }
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
            device.release(allocation, generation).map_err(|refusal| self.refuse(refusal))?;
            return Ok(Completion::Complete(0));
        }
        let Some((size, expected, expected_generation)) = self.pending
        else
        {
            // A retried ABORT can acknowledge only the hardware adapter's exact
            // previously completed token, never a different allocation or reuse.
            if operation == ABORT
            {
                device.abort(allocation, generation).map_err(|refusal| self.refuse(refusal))?;
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
            device.abort(allocation, generation).map_err(|refusal| self.refuse(refusal))?;
            self.pending = None;
            return Ok(Completion::Complete(0));
        }
        if output.len() < Outcome::BYTES
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        let outcome =
            device.commit(allocation, generation).map_err(|refusal| self.refuse(refusal))?;
        if outcome == Outcome::Applied
        {
            self.initial = None;
            self.attached.as_mut().ok_or(Error(mac_abi::NOT_FOUND))?.1 = size;
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
