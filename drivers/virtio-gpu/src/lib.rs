//! External Virtio Gpu video driver. The engine here is safe Rust: request
//! validation, the resize debounce and typed refusals. Device access lives in
//! one audited adapter (`machine`). System selects this driver by its VIDO
//! interface, never by file name.
#![no_std]
#![deny(unsafe_code)]
use mac_driver::{Completion, Error, Phase, Request, Result};
use mac_video_interface::{APPLY, ATTACH, Attach, CLASS, Damage, Outcome, Poll, SERVICE, Size, Surface};

/// The host window must keep one size this long before it is reported. Window
/// dragging produces a configuration change per mouse movement; only the size
/// that survives this quiet period reaches System.
pub const QUIET_MICROSECONDS: u64 = 250_000;

pub trait Hardware
{
    /// Map the driver-owned framebuffer, create the initial resource and scan
    /// it out. Reports the buffer and the host's preferred size.
    fn attach(&mut self, attach: &Attach) -> Result<Surface>;
    /// `Some` when the host reported a display change since the last call.
    fn host_size(&mut self) -> Result<Option<Size>>;
    /// Present the changed rectangle of the current mode.
    fn present(&mut self, damage: Damage) -> Result<()>;
    /// Create, scan out and present a new framebuffer resource. The previous
    /// mode stays active unless this returns `Outcome::Applied`.
    fn apply(&mut self, size: Size) -> Result<Outcome>;
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
    min:      Size,
    max:      Size,
    capacity: u64
}
pub struct Engine
{
    attached: Option<(Limits, Debounce)>
}
impl Engine
{
    pub const fn new() -> Self
    {
        Self { attached: None }
    }

    pub fn request(&mut self, device: &mut impl Hardware, request: Request<'_>,
                   output: &mut [u8])
                   -> Result<Completion>
    {
        if request.class != CLASS
        {
            return Err(Error(mac_abi::NOT_SUPPORTED));
        }
        if request.phase == Phase::Cancel
        {
            // Every operation completes synchronously; nothing can be cancelled.
            return Ok(Completion::Complete(0));
        }
        if request.phase != Phase::Start
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        match request.operation
        {
            ATTACH => self.attach(device, request.input, output),
            SERVICE => self.service(device, request.input, output),
            APPLY => self.apply(device, request.input, output),
            _ => Err(Error(mac_abi::NOT_SUPPORTED))
        }
    }

    fn attach(&mut self, device: &mut impl Hardware, input: &[u8], output: &mut [u8])
              -> Result<Completion>
    {
        let attach = Attach::decode(input).ok_or(Error(mac_abi::BAD_ARGUMENT))?;
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
        if output.len() < Surface::BYTES || !within(current, min, max)
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        let surface = device.attach(&attach)?;
        if bytes(current).is_none_or(|needed| needed > surface.capacity)
        {
            return Err(Error(mac_abi::BAD_ABI));
        }
        let now = device.microseconds()?;
        let mut debounce = Debounce::new(current);
        debounce.observe(surface.preferred, now);
        self.attached = Some((Limits { min,
                                       max,
                                       capacity: surface.capacity },
                              debounce));
        output[..Surface::BYTES].copy_from_slice(&surface.encode());
        Ok(Completion::Complete(Surface::BYTES))
    }

    fn service(&mut self, device: &mut impl Hardware, input: &[u8], output: &mut [u8])
               -> Result<Completion>
    {
        let damage = Damage::decode(input).ok_or(Error(mac_abi::BAD_ARGUMENT))?;
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
        if damage.width != 0 && damage.height != 0
        {
            device.present(damage)?;
        }
        let poll = Poll { settled: debounce.settled(now) };
        output[..Poll::BYTES].copy_from_slice(&poll.encode());
        Ok(Completion::Complete(Poll::BYTES))
    }

    fn apply(&mut self, device: &mut impl Hardware, input: &[u8], output: &mut [u8])
             -> Result<Completion>
    {
        let size = Size::decode(input).ok_or(Error(mac_abi::BAD_ARGUMENT))?;
        let (limits, debounce) = self.attached.as_mut().ok_or(Error(mac_abi::NOT_FOUND))?;
        if output.len() < Outcome::BYTES
        {
            return Err(Error(mac_abi::BAD_ARGUMENT));
        }
        let outcome = if size.width < limits.min.width || size.height < limits.min.height
        {
            Outcome::TooSmall
        }
        else if size.width > limits.max.width || size.height > limits.max.height
        {
            Outcome::TooLarge
        }
        else if bytes(size).is_none_or(|needed| needed > limits.capacity)
        {
            Outcome::ExceedsReservation
        }
        else
        {
            device.apply(size)?
        };
        if outcome == Outcome::Applied
        {
            debounce.applied(size);
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

#[cfg(target_arch = "riscv64")]
mod machine;
#[cfg(target_arch = "riscv64")]
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
#[cfg(target_arch = "riscv64")]
pub use native::DRIVER;
