//! Resident side of the VIDO video class: finds the one video provider,
//! hands it System's limits and presents accumulated damage. Policy for a
//! settled host size (clamping, mode switch, repaint, notification) stays in
//! the Toolbox; this module only moves typed requests.
use core::sync::atomic::{AtomicU64, Ordering};

use mac_abi::Status;
use mac_video_interface::{ABORT, APPLY, ATTACH, Attach, CLASS, COMMIT, Damage, Outcome, Poll,
                          Prepared, RELEASE, RESIZE, SERVICE, Size, Surface, VERSION};

use crate::display::{MAX_HEIGHT, MAX_WIDTH, MIN_HEIGHT, MIN_WIDTH, height, width};
use crate::toolbox::quickdraw::{Rect, geometry::union_rect};

struct Front
{
    allocation: u64,
    generation: u64,
    _buffer:    alloc::sync::Arc<super::dma::Buffer>
}
static CURRENT: spinning_top::Spinlock<Option<Front>> = spinning_top::Spinlock::new(None);

fn acknowledgment(allocation: u64, generation: u64) -> [u8; 16]
{
    let mut bytes = [0; 16];
    bytes[..8].copy_from_slice(&allocation.to_le_bytes());
    bytes[8..].copy_from_slice(&generation.to_le_bytes());
    bytes
}

fn validate_surface(surface: Surface, size: Size) -> Result<Front, Status>
{
    let module = crate::drivers::provider_module(PROVIDER.load(Ordering::Acquire))?;
    let buffer = super::driver_devices::pin_buffer(module, surface.allocation)?;
    let needed = u64::from(size.width).checked_mul(u64::from(size.height))
                                      .and_then(|bytes| bytes.checked_mul(4))
                                      .ok_or(mac_abi::BAD_ARGUMENT)?;
    if surface.generation == 0
       || surface.address != buffer.address()
       || surface.capacity != buffer.length()
       || needed > surface.capacity
       || surface.address % 4 != 0
    {
        return Err(mac_abi::BAD_ABI);
    }
    Ok(Front { allocation: surface.allocation,
               generation: surface.generation,
               _buffer:    buffer })
}

/// Publication uses the compositor's existing serialization, with only pointer,
/// geometry and owning-pin replacement under the native metadata gate. No
/// module callback or large clear runs while that gate is held.
fn publish(front: Front, surface: Surface, size: Size) -> Result<Option<Front>, Status>
{
    crate::display::validate(size.width as usize, size.height as usize).map_err(|_| {
                                                                           mac_abi::BAD_ARGUMENT
                                                                       })?;
    let previous = super::interrupts::with_masked(|| {
        // Validated immutable platform limits make this publication infallible.
        // Publish MODE before returning to any cursor or paint interrupt.
        let result = crate::display::set_mode(size.width as usize, size.height as usize);
        if result.is_err()
        {
            return Err(mac_abi::BAD_ABI);
        }
        super::display::use_driver_front(surface.address as usize);
        Ok(CURRENT.lock().replace(front))
    })?;
    // Existing compositor-owned scratch resets use the new live geometry.
    super::display::clear_mode_buffers();
    Ok(previous)
}

fn detach()
{
    let previous = super::interrupts::with_masked(|| {
        super::display::use_boot_front();
        CURRENT.lock().take()
    });
    drop(previous);
    crate::drivers::recovery_changed();
}

static ATTACH_PENDING: spinning_top::Spinlock<Option<Surface>> = spinning_top::Spinlock::new(None);
fn complete_attach(surface: Surface) -> Result<(), Status>
{
    let pin = validate_surface(surface,
                               Size { width:  width() as u32,
                                      height: height() as u32 })?;
    // Initialization can be retried after transient Device Manager contention.
    // Preserve the already visible scene when adopting its new DMA storage.
    super::display::copy_front_to(surface.address as usize);
    super::interrupts::with_masked(|| {
        *CURRENT.lock() = Some(pin);
        super::display::use_driver_front(surface.address as usize);
        *ATTACH_PENDING.lock() = None;
    });
    serial!("VIDEO_DRIVER_ATTACHED allocation={} generation={} front=0x{:x} capacity={} \
             preferred={}x{}",
            surface.allocation,
            surface.generation,
            surface.address,
            surface.capacity,
            surface.preferred.width,
            surface.preferred.height);
    super::display::mark_dirty(mac_abi::geometry::Rect::new(0, 0, width() as i16, height() as i16));
    Ok(())
}
fn provider_withdrawn() -> bool
{
    match crate::drivers::find_kernel_provider(CLASS, VERSION, RESIZE)
    {
        Ok(provider) => provider != PROVIDER.load(Ordering::Acquire),
        Err(mac_abi::NOT_FOUND | mac_abi::OPERATION_CANCELLED) => true,
        _ => false
    }
}

static ABORT_PENDING: spinning_top::Spinlock<Option<(u64, u64)>> =
    spinning_top::Spinlock::new(None);
static RELEASE_PENDING: spinning_top::Spinlock<Option<(u64, u64)>> =
    spinning_top::Spinlock::new(None);
fn abort(surface: Surface)
{
    match call(ABORT, &acknowledgment(surface.allocation, surface.generation), 0)
    {
        Ok(_) => (),
        Err(status) =>
        {
            *ABORT_PENDING.lock() = Some((surface.allocation, surface.generation));
            serial!("VIDEO_ABORT_PENDING allocation={} generation={} status={status}",
                    surface.allocation, surface.generation);
        }
    }
}

static PROVIDER: AtomicU64 = AtomicU64::new(0);
static LAST_SERVICE: AtomicU64 = AtomicU64::new(0);
/// 10 MHz time base: presenting damage waits at most 8 ms, an idle poll for
/// host resize events 50 ms.
const DAMAGE_INTERVAL: u64 = 80_000;
const IDLE_INTERVAL: u64 = 500_000;

pub(crate) fn attached() -> bool
{
    super::display::video_attached()
}

fn call(operation: u32, input: &[u8], output: usize) -> Result<alloc::vec::Vec<u8>, Status>
{
    let provider = PROVIDER.load(Ordering::Acquire);
    let mut owned = alloc::vec::Vec::new();
    owned.try_reserve_exact(input.len()).map_err(|_| mac_abi::NO_MEMORY)?;
    owned.extend_from_slice(input);
    let subject = crate::drivers::jobs::subjects::resident()?.admit(
        &crate::toolbox::handles::subject::Authority::Client(crate::toolbox::handles::system_owner()?),
        crate::allocation::Target::System(crate::allocation::SystemClass::Ordinary))?;
    let mut job = crate::drivers::submit_background_to(&subject, provider, CLASS, VERSION,
                                                       operation, owned, output)?;
    let completed = job.run_once()?.ok_or(mac_abi::SERVICE_BUSY)?;
    match completed.result
    {
        Ok(bytes) => Ok(bytes),
        Err(crate::drivers::jobs::Failure::Service(status)) => Err(status),
        Err(crate::drivers::jobs::Failure::Cancelled) => Err(mac_abi::OPERATION_CANCELLED)
    }
}

/// Attaches the installed video driver, if any, and switches System's
/// scan-out to the memory it owns. `Ok(false)` means no provider is installed
/// and the ramfb path stays in use.
pub(crate) fn attach() -> Result<bool, Status>
{
    let pending = super::interrupts::with_masked(|| *ATTACH_PENDING.lock());
    if let Some(surface) = pending
    {
        complete_attach(surface)?;
        return Ok(true);
    }
    let provider = match crate::drivers::find_kernel_provider(CLASS, VERSION, RESIZE)
    {
        Ok(provider) => provider,
        Err(mac_abi::NOT_FOUND) => return Ok(false),
        Err(error) => return Err(error)
    };
    PROVIDER.store(provider, Ordering::Release);
    let request = Attach { width:      width() as u32,
                           height:     height() as u32,
                           min_width:  MIN_WIDTH as u32,
                           min_height: MIN_HEIGHT as u32,
                           max_width:  MAX_WIDTH as u32,
                           max_height: MAX_HEIGHT as u32 };
    let reply = call(ATTACH, &request.encode(), Surface::BYTES)?;
    let surface = Surface::decode(&reply).ok_or(mac_abi::BAD_ABI)?;
    *ATTACH_PENDING.lock() = Some(surface);
    complete_attach(surface)?;
    Ok(true)
}

/// Presents pending damage and reports a settled host size, at most as often
/// as the intervals above allow. `Ok(None)` also covers "not due yet".
pub(crate) fn service() -> Result<Option<Size>, Status>
{
    if !attached()
    {
        let pending = super::interrupts::with_masked(|| *ATTACH_PENDING.lock());
        if let Some(surface) = pending
        {
            match complete_attach(surface)
            {
                Ok(()) => (),
                Err(status) =>
                {
                    if provider_withdrawn()
                    {
                        *ATTACH_PENDING.lock() = None;
                    }
                    return Err(status);
                }
            }
        }
        else
        {
            let now = super::display::ticks();
            if PROVIDER.load(Ordering::Acquire) != 0
               && now.wrapping_sub(LAST_SERVICE.load(Ordering::Relaxed)) >= IDLE_INTERVAL
            {
                LAST_SERVICE.store(now, Ordering::Relaxed);
                attach()?;
            }
            return Ok(None);
        }
    }
    let now = super::display::ticks();
    let pending = super::display::peek_dirty();
    let interval = if pending { DAMAGE_INTERVAL } else { IDLE_INTERVAL };
    if now.wrapping_sub(LAST_SERVICE.load(Ordering::Relaxed)) < interval
    {
        return Ok(None);
    }
    LAST_SERVICE.store(now, Ordering::Relaxed);
    let abort_debt = super::interrupts::with_masked(|| *ABORT_PENDING.lock());
    if let Some((allocation, generation)) = abort_debt
    {
        match call(ABORT, &acknowledgment(allocation, generation), 0)
        {
            Ok(_) =>
            {
                *ABORT_PENDING.lock() = None;
            }
            Err(status) =>
            {
                if provider_withdrawn()
                {
                    *ABORT_PENDING.lock() = None;
                    detach();
                    return Ok(None);
                }
                return Err(status);
            }
        }
    }
    let release_debt = super::interrupts::with_masked(|| *RELEASE_PENDING.lock());
    if let Some((allocation, generation)) = release_debt
    {
        match call(RELEASE, &acknowledgment(allocation, generation), 0)
        {
            Ok(_) =>
            {
                *RELEASE_PENDING.lock() = None;
                serial!("VIDEO_OLD_SURFACE_RELEASED allocation={allocation} \
                         generation={generation}");
            }
            Err(status) =>
            {
                if provider_withdrawn()
                {
                    *RELEASE_PENDING.lock() = None;
                    detach();
                    return Ok(None);
                }
                return Err(status);
            }
        }
    }
    let dirty = super::display::take_dirty();
    let mut input = [0; Damage::BYTES * Damage::MAX_RECTS];
    for (chunk, rect) in input.chunks_exact_mut(Damage::BYTES).zip(dirty.rects())
    {
        let damage = Damage { x:      rect.left as u32,
                              y:      rect.top as u32,
                              width:  (rect.right - rect.left) as u32,
                              height: (rect.bottom - rect.top) as u32 };
        chunk.copy_from_slice(&damage.encode());
    }
    let length = dirty.rects().len() * Damage::BYTES;
    match call(SERVICE, &input[..length], Poll::BYTES)
        .and_then(|reply| Poll::decode(&reply).ok_or(mac_abi::BAD_ABI))
    {
        Ok(poll) => Ok(poll.settled),
        Err(status) =>
        {
            if provider_withdrawn()
            {
                detach();
            }
            // Keep the damage so the next successful call presents it.
            super::display::restore_dirty(dirty);
            Err(status)
        }
    }
}

pub(crate) fn apply(size: Size) -> Result<Outcome, Status>
{
    if super::interrupts::with_masked(|| {
        ABORT_PENDING.lock().is_some() || RELEASE_PENDING.lock().is_some()
    })
    {
        return Err(mac_abi::SERVICE_BUSY);
    }
    crate::display::validate(size.width as usize, size.height as usize).map_err(|_| {
                                                                           mac_abi::BAD_ARGUMENT
                                                                       })?;
    let reply = call(APPLY, &size.encode(), Prepared::BYTES)?;
    let prepared = Prepared::decode(&reply).ok_or(mac_abi::BAD_ABI)?;
    if prepared.outcome != Outcome::Applied
    {
        return Ok(prepared.outcome);
    }
    let surface = prepared.surface.ok_or(mac_abi::BAD_ABI)?;
    let pin = match validate_surface(surface, size)
    {
        Ok(pin) => pin,
        Err(cause) =>
        {
            // No new CPU view escaped validation. An ABORT refusal retains the
            // driver's exact candidate and original cleanup/account ownership.
            abort(surface);
            return Err(cause);
        }
    };
    let reply =
        match call(COMMIT, &acknowledgment(surface.allocation, surface.generation), Outcome::BYTES)
        {
            Ok(reply) => reply,
            Err(cause) =>
            {
                drop(pin);
                abort(surface);
                return Err(cause);
            }
        };
    let outcome = Outcome::from_code(u32::from_le_bytes(reply.as_slice().try_into().map_err(|_| mac_abi::BAD_ABI)?)).ok_or(mac_abi::BAD_ABI)?;
    if outcome != Outcome::Applied
    {
        drop(pin);
        abort(surface);
        return Ok(outcome);
    }
    let previous = publish(pin, surface, size)?;
    serial!("VIDEO_SURFACE_COMMITTED allocation={} generation={} front=0x{:x} capacity={} \
             mode={}x{}",
            surface.allocation,
            surface.generation,
            surface.address,
            surface.capacity,
            size.width,
            size.height);
    if let Some(previous) = previous
    {
        let allocation = previous.allocation;
        let generation = previous.generation;
        // The driver cannot reclaim old storage until this original CPU pin
        // ends, even if the GPU has already stopped reading the old resource.
        drop(previous);
        crate::drivers::recovery_changed();
        match call(RELEASE, &acknowledgment(allocation, generation), 0)
        {
            Ok(_) =>
            {
                serial!("VIDEO_OLD_SURFACE_RELEASED allocation={allocation} \
                         generation={generation}")
            }
            Err(status) =>
            {
                *RELEASE_PENDING.lock() = Some((allocation, generation));
                serial!("VIDEO_OLD_SURFACE_PENDING allocation={allocation} \
                         generation={generation} status={status}");
            }
        }
    }
    super::display::mark_dirty(mac_abi::geometry::Rect::new(0, 0, size.width as i16,
                                                            size.height as i16));
    Ok(Outcome::Applied)
}

/// Accumulated damage for the next present, bounded to one message.
///
/// This is shared rather than per-architecture: it is data over `Rect` that asks
/// the display for its bounds, and the shared present path consumes it. It lived
/// in the RISC-V adapter only because nothing else compiled the present path.
pub(crate) struct DamageRegion
{
    rects: [Rect; mac_video_interface::Damage::MAX_RECTS],
    pub(crate) len: usize
}

impl DamageRegion
{
    pub(crate) const EMPTY: Self = Self { rects: [Rect::ZERO; mac_video_interface::Damage::MAX_RECTS],
                               len:   0 };

    pub(crate) fn add(&mut self, mut rect: Rect)
    {
        rect = Rect::new(rect.left.max(0),
                         rect.top.max(0),
                         rect.right.min(width() as i16),
                         rect.bottom.min(height() as i16));
        if rect.is_empty()
        {
            return;
        }

        // Merge only when the union still describes exactly the same pixels.
        let mut i = 0;
        while i < self.len
        {
            let old = self.rects[i];
            if old.left <= rect.left
               && old.top <= rect.top
               && old.right >= rect.right
               && old.bottom >= rect.bottom
            {
                return;
            }
            let bounds = union_rect(old, rect);
            if bounds == rect
               || (old.left == rect.left
                   && old.right == rect.right
                   && old.top <= rect.bottom
                   && rect.top <= old.bottom)
               || (old.top == rect.top
                   && old.bottom == rect.bottom
                   && old.left <= rect.right
                   && rect.left <= old.right)
            {
                rect = bounds;
                self.len -= 1;
                self.rects[i] = self.rects[self.len];
                i = 0;
            }
            else
            {
                i += 1;
            }
        }
        if self.len == self.rects.len()
        {
            // Bound the backlog if the display falls behind a complex scene.
            // Keep all pending pixels covered, without forcing a mode-wide update.
            rect = self.rects().iter().copied().fold(rect, union_rect);
            self.len = 0;
        }
        self.rects[self.len] = rect;
        self.len += 1;
    }

    pub(crate) fn rects(&self) -> &[Rect]
    {
        &self.rects[..self.len]
    }
}

