//! Bootstrap owns reserved pixels and an early scanout. Only the primary boot
//! hart calls this module; no storage callback or monitor entry draws here.
use core::{cell::UnsafeCell,
           fmt::{self, Write}};

use mac_boot_screen::Raster;

use crate::{memory::{DISPLAY_HEIGHT, DISPLAY_SCRATCH_BASE, DISPLAY_SCRATCH_BYTES, DISPLAY_WIDTH},
            virtio_gpu};
const WIDTH: usize = DISPLAY_WIDTH as usize;
const HEIGHT: usize = DISPLAY_HEIGHT as usize;
const _: () = assert!(DISPLAY_SCRATCH_BYTES >= (WIDTH * HEIGHT * size_of::<u32>()) as u64);
#[derive(Debug)]
pub enum Error
{
    Memory(mac_abi::boot::MemoryError),
    Gpu(virtio_gpu::Failure),
    Ramfb(mac_boot_screen::ramfb::Error)
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Memory(e) => e.fmt(f),
            Self::Gpu(e) => e.fmt(f),
            Self::Ramfb(e) => e.fmt(f)
        }
    }
}
enum Backend
{
    None,
    Gpu(virtio_gpu::Device),
    Ramfb
}
struct State
{
    backend:    Backend,
    handed_off: bool
}
struct Shared(UnsafeCell<State>);
// SAFETY: the boot hart is the only graphics producer; secondary harts remain
// parked, and firmware disk/monitor callbacks cannot enter this private module.
// The one-way handoff prevents any subsequent ROM display/DMA access.
unsafe impl Sync for Shared
{
}
static STATE: Shared = Shared(UnsafeCell::new(State { backend:    Backend::None,
                                                      handed_off: false }));
fn raster() -> Option<Raster<'static>>
{
    // Boot-only serialized, initialized reserved physical memory. No view
    // escapes a drawing function or remains live during a GPU transfer.
    let pixels = unsafe {
        core::slice::from_raw_parts_mut(DISPLAY_SCRATCH_BASE as *mut u32, WIDTH * HEIGHT)
    };
    Raster::new(pixels, WIDTH, HEIGHT)
}
fn ramfb(state: &mut State) -> Result<bool, Error>
{
    let result = unsafe {
        mac_boot_screen::ramfb::configure(crate::memory::FW_CFG_BASE as usize,
                                          DISPLAY_SCRATCH_BASE,
                                          WIDTH,
                                          HEIGHT,
                                          |pointer, _| Some(pointer as u64))
    };
    match result
    {
        Ok(()) =>
        {
            state.backend = Backend::Ramfb;
            let _ = writeln!(crate::Serial,
                             "BOOT_VIDEO_READY backend=ramfb width={WIDTH} height={HEIGHT} \
                              frame={DISPLAY_SCRATCH_BASE:#x}");
            Ok(true)
        }
        Err(error) =>
        {
            let _ = writeln!(crate::Serial, "BOOT_RAMFB_REJECTED: {error}");
            state.backend = Backend::None;
            if error.dma_ownership_unknown() { Err(Error::Ramfb(error)) } else { Ok(false) }
        }
    }
}
pub fn init(memory: &crate::memory::Memory, timebase_hz: u64) -> Result<bool, Error>
{
    let memory =
        mac_abi::boot::Memory { ram:          &memory.ram,
                                reservations: &memory.reserved }.checked(crate::memory::PAGE_BYTES,
                                                                         memory.ram[0])
                                                                .map_err(Error::Memory)?;
    // Admission precedes the first physical framebuffer write, not just DMA.
    memory.require_reservation(mac_abi::BootMemoryRange { base:   DISPLAY_SCRATCH_BASE,
                                                          length: DISPLAY_SCRATCH_BYTES })
          .map_err(Error::Memory)?;
    let state = unsafe { &mut *STATE.0.get() };
    if state.handed_off
    {
        return Ok(false);
    }
    if !matches!(state.backend, Backend::None)
    {
        return Ok(true);
    }
    unsafe { core::ptr::write_bytes(DISPLAY_SCRATCH_BASE as *mut u32, 0, WIDTH * HEIGHT) };
    if let Some(mut canvas) = raster()
    {
        canvas.background();
    }
    else
    {
        return Ok(false);
    }
    for slot in 0..crate::memory::VIRTIO_MMIO_SLOTS
    {
        let base =
            (crate::memory::VIRTIO_MMIO_BASE + slot * crate::memory::VIRTIO_MMIO_STRIDE) as usize;
        if !virtio_gpu::is_gpu(base)
        {
            continue;
        }
        match virtio_gpu::Device::open(base, DISPLAY_SCRATCH_BASE, WIDTH as u32, HEIGHT as u32,
                                       timebase_hz, memory)
        {
            Ok(gpu) =>
            {
                state.backend = Backend::Gpu(gpu);
                let _ = writeln!(crate::Serial,
                                 "BOOT_VIDEO_READY backend=virtio-gpu mmio={base:#x} \
                                  width={WIDTH} height={HEIGHT} frame={DISPLAY_SCRATCH_BASE:#x}");
                return Ok(true);
            }
            Err(error) =>
            {
                let _ = writeln!(crate::Serial, "BOOT_VIRTIO_GPU_REJECTED: {error}");
                if error.reset.is_some()
                {
                    state.handed_off = true;
                    return Err(Error::Gpu(error));
                }
                // No DMA was submitted, or actual reset acknowledged cessation.
                // Only then may RAMFB use the same reserved pixels.
                return ramfb(state);
            }
        }
    }
    let _ = writeln!(crate::Serial, "BOOT_VIRTIO_GPU_ABSENT: selecting RAMFB bootstrap");
    ramfb(state)
}
fn icon(bytes: &[u8; 256]) -> Result<bool, Error>
{
    let state = unsafe { &mut *STATE.0.get() };
    if state.handed_off
    {
        return Ok(false);
    }
    let left = (WIDTH - 32) / 2;
    let top = (HEIGHT - 32) / 2;
    if let Some(mut canvas) = raster()
    {
        canvas.icon(bytes, left as i16, top as i16, 1);
    }
    else
    {
        return Ok(false);
    }
    if let Backend::Gpu(gpu) = &mut state.backend
    {
        if let Err(error) = gpu.update(left as u32, top as u32, 32, 32)
        {
            let _ = writeln!(crate::Serial, "BOOT_VIRTIO_GPU_REJECTED: {error}");
            if error.reset.is_some()
            {
                state.handed_off = true;
                return Err(Error::Gpu(error));
            }
            state.backend = Backend::None;
            return ramfb(state);
        }
    }
    Ok(!matches!(state.backend, Backend::None))
}
pub fn happy() -> Result<bool, Error>
{
    icon(mac_boot_screen::HAPPY)
}
pub fn missing() -> Result<bool, Error>
{
    icon(mac_boot_screen::MISSING)
}
/// Preserve the last completed image and the original boot owner. Both DMA
/// areas remain excluded from every later allocator. The runtime GPU driver's
/// genuine VDEV reset must acknowledge before installing its own control queue
/// and backing. No firmware graphics code runs again, even on disk callbacks.
pub fn handoff()
{
    let state = unsafe { &mut *STATE.0.get() };
    state.handed_off = true;
    match &state.backend
    {
        Backend::Gpu(gpu) => gpu.handoff(),
        Backend::Ramfb =>
        {
            let _ = writeln!(crate::Serial,
                             "BOOT_VIDEO_HANDOFF backend=ramfb reserved_until_reset=true");
        }
        Backend::None => ()
    }
}
