//! Resident side of the VIDO video class: finds the one video provider,
//! hands it System's limits and presents accumulated damage. Policy for a
//! settled host size (clamping, mode switch, repaint, notification) stays in
//! the Toolbox; this module only moves typed requests.
use core::sync::atomic::{AtomicU64, Ordering};

use mac_abi::Status;
use mac_video_interface::{APPLY, ATTACH, Attach, CLASS, Damage, Outcome, Poll, RESIZE, SERVICE,
                          Size, Surface, VERSION};

use crate::display::{MAX_HEIGHT, MAX_WIDTH, MIN_HEIGHT, MIN_WIDTH, height, width};

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

fn call(operation: u32, input: alloc::vec::Vec<u8>, output: usize)
        -> Result<alloc::vec::Vec<u8>, Status>
{
    let provider = PROVIDER.load(Ordering::Acquire);
    let subject = crate::drivers::jobs::subjects::resident()?.admit(
        &crate::toolbox::handles::subject::Authority::Client(crate::toolbox::handles::system_owner()?),
        crate::allocation::Target::System(crate::allocation::SystemClass::Ordinary))?;
    let mut job =
        crate::drivers::submit_background_to(&subject, provider, CLASS, VERSION, operation, input,
                                             output)?;
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
    let reply = call(ATTACH, request.encode().to_vec(), Surface::BYTES)?;
    let surface = Surface::decode(&reply).ok_or(mac_abi::BAD_ABI)?;
    if surface.address == 0
       || surface.address % 4 != 0
       || surface.capacity < (MAX_WIDTH * MAX_HEIGHT * 4) as u64
    {
        serial!("VIDEO_DRIVER_REFUSED capacity={} required={}",
                surface.capacity,
                MAX_WIDTH * MAX_HEIGHT * 4);
        return Err(mac_abi::BAD_ABI);
    }
    serial!("VIDEO_DRIVER_ATTACHED front=0x{:x} capacity={} preferred={}x{}",
            surface.address,
            surface.capacity,
            surface.preferred.width,
            surface.preferred.height);
    super::display::use_driver_front(surface.address as usize);
    super::display::mark_dirty(mac_abi::geometry::Rect::new(0, 0, width() as i16, height() as i16));
    Ok(true)
}

/// Presents pending damage and reports a settled host size, at most as often
/// as the intervals above allow. `Ok(None)` also covers "not due yet".
pub(crate) fn service() -> Result<Option<Size>, Status>
{
    if !attached()
    {
        return Ok(None);
    }
    let now = super::display::ticks();
    let pending = super::display::peek_dirty();
    let interval = if pending { DAMAGE_INTERVAL } else { IDLE_INTERVAL };
    if now.wrapping_sub(LAST_SERVICE.load(Ordering::Relaxed)) < interval
    {
        return Ok(None);
    }
    LAST_SERVICE.store(now, Ordering::Relaxed);
    let rect = super::display::take_dirty();
    let damage = rect.map_or(Damage::default(), |rect| Damage { x:      rect.left as u32,
                                                                y:      rect.top as u32,
                                                                width:  (rect.right - rect.left) as u32,
                                                                height: (rect.bottom - rect.top) as u32 });
    match call(SERVICE, damage.encode().to_vec(), Poll::BYTES)
    {
        Ok(reply) => Ok(Poll::decode(&reply).ok_or(mac_abi::BAD_ABI)?.settled),
        Err(status) =>
        {
            // Keep the damage so the next successful call presents it.
            if let Some(rect) = rect
            {
                super::display::mark_dirty(rect);
            }
            Err(status)
        }
    }
}

pub(crate) fn apply(size: Size) -> Result<Outcome, Status>
{
    let reply = call(APPLY, size.encode().to_vec(), Outcome::BYTES)?;
    let code = u32::from_le_bytes(reply.as_slice().try_into().map_err(|_| mac_abi::BAD_ABI)?);
    Outcome::from_code(code).ok_or(mac_abi::BAD_ABI)
}
