//! QEMU virt display and clock; System owns the framebuffers.
use core::{arch::asm,
           sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}};

/// Live display geometry comes from `crate::display`; the platform record only
/// fixes the default and the maximum the reservations are sized for.
use crate::display::{height, pixels, width};

static BACKBUFFER_INITIALIZED: AtomicBool = AtomicBool::new(false);
// The complete physical display reservation remains build-owned and excluded
// from the page pool. Only the live front/back mapped views are used here.
const _: () = assert!(crate::machine::memory::DISPLAY_BACKBUFFER_BASE
                      + (crate::display::MAX_PIXELS * size_of::<u32>()) as u64
                      <= crate::machine::memory::DISPLAY_SCRATCH_BASE
                         + crate::machine::memory::DISPLAY_SCRATCH_BYTES);

// The checked platform window covers minimum RAM; native admission proves all
// discovered RAM is mapped before any framebuffer view is created. These are
// first mapped views, not rebased physical references or fallible paint lookups.
const FRONT_CPU: usize = (crate::machine::memory::VM_PHYSMAP_START
                          + (crate::machine::memory::DISPLAY_FRONTBUFFER_BASE
                             - crate::machine::memory::VM_PHYSMAP_PHYSICAL_BASE))
                         as usize;
const BACK_CPU: usize = (crate::machine::memory::VM_PHYSMAP_START
                         + (crate::machine::memory::DISPLAY_BACKBUFFER_BASE
                            - crate::machine::memory::VM_PHYSMAP_PHYSICAL_BASE))
                        as usize;
/// Scan-out memory System draws into: the platform reservation for ramfb, or
/// the video driver's own framebuffer once one attaches.
static FRONT: AtomicUsize = AtomicUsize::new(FRONT_CPU);
/// Union of scan-out rectangles changed since the video driver last presented,
/// packed as four i16 (left, top, right, bottom); zero is empty.
static DIRTY: AtomicU64 = AtomicU64::new(0);
static VIDEO_ATTACHED: AtomicBool = AtomicBool::new(false);

fn front_cpu() -> usize
{
    FRONT.load(Ordering::Acquire)
}

pub(crate) fn use_driver_front(address: usize)
{
    FRONT.store(address, Ordering::Release);
    VIDEO_ATTACHED.store(true, Ordering::Release);
}

/// Caller holds the authenticated new allocation pin and compositor admission.
/// No data reference escapes this one scene copy.
pub(crate) fn copy_front_to(address: usize)
{
    if address != front_cpu()
    {
        // SAFETY: source is the current pinned front, destination is a distinct
        // validated runtime allocation covering the current complete mode.
        unsafe {
            core::ptr::copy_nonoverlapping(front_cpu() as *const u32,
                                           address as *mut u32,
                                           pixels());
        }
    }
}

pub(crate) fn use_boot_front()
{
    FRONT.store(FRONT_CPU, Ordering::Release);
    VIDEO_ATTACHED.store(false, Ordering::Release);
}

pub(crate) fn video_attached() -> bool
{
    VIDEO_ATTACHED.load(Ordering::Acquire)
}

fn pack(rect: mac_abi::geometry::Rect) -> u64
{
    u64::from(rect.left as u16)
    | u64::from(rect.top as u16) << 16
    | u64::from(rect.right as u16) << 32
    | u64::from(rect.bottom as u16) << 48
}

pub(crate) fn mark_dirty(rect: mac_abi::geometry::Rect)
{
    if rect.right <= rect.left || rect.bottom <= rect.top || !video_attached()
    {
        return;
    }
    let _ = DIRTY.try_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                     if old == 0
                     {
                         return Some(pack(rect));
                     }
                     let at = |shift: u32| (old >> shift) as u16 as i16;
                     Some(pack(mac_abi::geometry::Rect::new(at(0).min(rect.left),
                                                            at(16).min(rect.top),
                                                            at(32).max(rect.right),
                                                            at(48).max(rect.bottom))))
                 });
}

pub(crate) fn peek_dirty() -> bool
{
    DIRTY.load(Ordering::Acquire) != 0
}

/// Takes the accumulated changed rectangle clipped to the live mode.
pub(crate) fn take_dirty() -> Option<mac_abi::geometry::Rect>
{
    let packed = DIRTY.swap(0, Ordering::AcqRel);
    if packed == 0
    {
        return None;
    }
    let at = |shift: u32| (packed >> shift) as u16 as i16;
    let rect = mac_abi::geometry::Rect::new(at(0).max(0),
                                            at(16).max(0),
                                            at(32).min(width() as i16),
                                            at(48).min(height() as i16));
    (rect.right > rect.left && rect.bottom > rect.top).then_some(rect)
}

pub fn back_buffer() -> &'static mut [u32]
{
    let pointer = BACK_CPU as *mut u32;
    // Bootstrap initializes this before applications start. Afterwards only the
    // interrupt-masked, serialized compositor borrows the dedicated reservation.
    // It is disjoint from the frontbuffer.
    if !BACKBUFFER_INITIALIZED.load(Ordering::Acquire)
    {
        unsafe {
            core::ptr::write_bytes(pointer, 0, pixels());
        }
        BACKBUFFER_INITIALIZED.store(true, Ordering::Release);
    }
    unsafe { core::slice::from_raw_parts_mut(pointer, pixels()) }
}
/// The compositor owns these temporary borrows. Pointer/geometry publication
/// is a separate bounded metadata transaction, never a large IRQ-off clear.
pub(crate) fn clear_mode_buffers()
{
    // SAFETY: current front is held by video::CURRENT or permanent ramfb;
    // all spans admit the current validated geometry, and Desktop serializes
    // painting. The front-buffer pin is retained through this entire clear.
    unsafe {
        core::ptr::write_bytes(BACK_CPU as *mut u32, 0, pixels());
        core::ptr::write_bytes(front_cpu() as *mut u32, 0, pixels());
    }
}
pub fn setup_framebuffer() -> bool
{
    let address = crate::machine::memory::DISPLAY_FRONTBUFFER_BASE;
    // The checked platform frontbuffer reservation is pinned for System's lifetime; bootstrap owns
    // exclusive fw_cfg access, before any application or secondary hart starts.
    // The binding translates the permanent high-linked System scratch to bus
    // PA; shared device code never treats a CPU pointer as a physical address.
    let result = unsafe {
        mac_boot_screen::ramfb::configure(crate::machine::memory::FW_CFG_BASE as usize,
                                          address,
                                          width(),
                                          height(),
                                          |pointer, bytes| {
                                              crate::machine::paging::bus_address(pointer as u64,
                                                                         bytes as u64)
                                          })
    };
    if let Err(error) = &result
    {
        serial!("RAMFB_REJECTED: {error}");
    }
    // Without ramfb System renders into its reserved memory until a video
    // driver attaches; boot fails later if none does.
    let ready = result.is_ok();
    if !ready
    {
        serial!("DISPLAY_NO_RAMFB: continuing until a video driver attaches");
    }
    if ready
    {
        serial!("RAMFB {}x{} addr=0x{:x}", width(), height(), address);
        serial!("DISPLAY_BUFFERS back={} front={} bytes={}",
                crate::machine::memory::DISPLAY_BACKBUFFER_BASE,
                address,
                pixels() * size_of::<u32>());
    }
    ready
}

pub fn ticks() -> u64
{
    {
        let value;
        unsafe {
            asm!("rdtime {}",out(reg)value,options(nomem,nostack));
        }
        value
    }
}
pub fn present(back: &[u32])
{
    present_rects(back,
                  [mac_abi::geometry::Rect::new(0, 0, width() as i16, height() as i16)],
                  Default::default());
}

/// Copy real scene damage and apply frontbuffer-only cursor/drag transitions.
pub(crate) fn present_rects(back: &[u32],
                            rects: impl IntoIterator<Item = mac_abi::geometry::Rect>,
                            overlays: crate::toolbox::quickdraw::presentation::Overlays<'_>)
{
    use crate::toolbox::quickdraw::presentation::{PresentEvent, present_frame};
    // The serialized compositor owns both buffers. This exclusive frontbuffer
    // slice cannot escape; the scene stays read-only for the entire transition.
    let front = unsafe { core::slice::from_raw_parts_mut(front_cpu() as *mut u32, pixels()) };
    if video_attached()
    {
        // Overlays change the frontbuffer without scene damage. Cursor pixels
        // are bounded; outline feedback is not, so it presents the whole mode.
        mark_dirty(overlays.previous_cursor);
        if let Some(cursor) = overlays.cursor
        {
            mark_dirty(cursor.bounds());
        }
        if overlays.feedback.is_some() || overlays.previous_feedback.is_some()
        {
            mark_dirty(mac_abi::geometry::Rect::new(0, 0, width() as i16, height() as i16));
        }
    }
    let mut started = 0;
    let owner = crate::context::current_thread_context().identity().app.0;
    present_frame(back, front, width(), height(), rects, overlays, |event| {
        match event
        {
            PresentEvent::CopyStarted | PresentEvent::OutlineStarted =>
            {
                started = crate::trace::clock()
            }
            PresentEvent::Copied(rect) =>
            {
                mark_dirty(rect);
                crate::trace::record(crate::trace::FRAME_COPY,
                                     owner,
                                     crate::trace::clock().wrapping_sub(started),
                                     (rect.right - rect.left) as u64
                                     * (rect.bottom - rect.top) as u64,
                                     ((rect.left as u64) << 32) | rect.top as u64,
                                     ((rect.right as u64) << 32) | rect.bottom as u64)
            }
            PresentEvent::OutlineChanged(pixels) if pixels != 0 =>
            {
                crate::trace::record(crate::trace::WINDOW,
                                     owner,
                                     crate::trace::WINDOW_OUTLINE_XOR,
                                     pixels as u64,
                                     crate::trace::clock().wrapping_sub(started),
                                     0)
            }
            PresentEvent::OutlineChanged(_) => ()
        }
    });
    unsafe {
        asm!("fence iorw, iorw", options(nostack));
    }
}
