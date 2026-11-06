//! QEMU virt display and clock; System owns the framebuffers.
use core::{arch::asm,
           sync::atomic::{AtomicBool, AtomicUsize, Ordering}};

/// Live display geometry comes from `crate::display`; the platform record only
/// fixes the default and the maximum the reservations are sized for.
use crate::display::{height, pixels, width};
use crate::toolbox::quickdraw::{Rect, geometry::union_rect};

static BACKBUFFER_INITIALIZED: AtomicBool = AtomicBool::new(false);
// The complete physical display reservation remains build-owned and excluded
// from the page pool. Only the live front/back mapped views are used here.
const _: () = assert!(super::memory::DISPLAY_BACKBUFFER_BASE
                      + (crate::display::MAX_PIXELS * size_of::<u32>()) as u64
                      <= super::memory::DISPLAY_SCRATCH_BASE
                         + super::memory::DISPLAY_SCRATCH_BYTES);

// The checked platform window covers minimum RAM; native admission proves all
// discovered RAM is mapped before any framebuffer view is created. These are
// first mapped views, not rebased physical references or fallible paint lookups.
const FRONT_CPU: usize = (super::memory::VM_PHYSMAP_START
                          + (super::memory::DISPLAY_FRONTBUFFER_BASE
                             - super::memory::VM_PHYSMAP_PHYSICAL_BASE))
                         as usize;
const BACK_CPU: usize = (super::memory::VM_PHYSMAP_START
                         + (super::memory::DISPLAY_BACKBUFFER_BASE
                            - super::memory::VM_PHYSMAP_PHYSICAL_BASE))
                        as usize;
/// Scan-out memory System draws into: the platform reservation for ramfb, or
/// the video driver's own framebuffer once one attaches.
static FRONT: AtomicUsize = AtomicUsize::new(FRONT_CPU);
/// Scan-out damage accumulated until the next video service call.
static DIRTY: spinning_top::Spinlock<DamageRegion> =
    spinning_top::Spinlock::new(DamageRegion::EMPTY);
static VIDEO_ATTACHED: AtomicBool = AtomicBool::new(false);

fn front_cpu() -> usize
{
    FRONT.load(Ordering::Acquire)
}

pub(super) fn use_driver_front(address: usize)
{
    FRONT.store(address, Ordering::Release);
    VIDEO_ATTACHED.store(true, Ordering::Release);
}

/// Caller holds the authenticated new allocation pin and compositor admission.
/// No data reference escapes this one scene copy.
pub(super) fn copy_front_to(address: usize)
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

pub(super) fn use_boot_front()
{
    FRONT.store(FRONT_CPU, Ordering::Release);
    VIDEO_ATTACHED.store(false, Ordering::Release);
    *DIRTY.lock() = DamageRegion::EMPTY;
}

pub(super) fn video_attached() -> bool
{
    VIDEO_ATTACHED.load(Ordering::Acquire)
}

pub(super) struct DamageRegion
{
    rects: [Rect; mac_video_interface::Damage::MAX_RECTS],
    len:   usize
}

impl DamageRegion
{
    const EMPTY: Self = Self { rects: [Rect::ZERO; mac_video_interface::Damage::MAX_RECTS],
                               len:   0 };

    fn add(&mut self, mut rect: Rect)
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

    pub(super) fn rects(&self) -> &[Rect]
    {
        &self.rects[..self.len]
    }
}

pub(super) fn mark_dirty(rect: Rect)
{
    super::interrupts::with_masked(|| {
        if video_attached()
        {
            DIRTY.lock().add(rect);
        }
    });
}

pub(super) fn restore_dirty(region: DamageRegion)
{
    if region.len == 0
    {
        return;
    }
    super::interrupts::with_masked(|| {
        if video_attached()
        {
            let mut dirty = DIRTY.lock();
            if dirty.len == 0
            {
                *dirty = region;
            }
            else
            {
                for &rect in region.rects()
                {
                    dirty.add(rect);
                }
            }
        }
    });
}

pub(super) fn peek_dirty() -> bool
{
    super::interrupts::with_masked(|| DIRTY.lock().len != 0)
}

pub(super) fn take_dirty() -> DamageRegion
{
    super::interrupts::with_masked(|| core::mem::replace(&mut *DIRTY.lock(), DamageRegion::EMPTY))
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
pub(super) fn clear_mode_buffers()
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
    let address = super::memory::DISPLAY_FRONTBUFFER_BASE;
    // The checked platform frontbuffer reservation is pinned for System's lifetime; bootstrap owns
    // exclusive fw_cfg access, before any application or secondary hart starts.
    // The binding translates the permanent high-linked System scratch to bus
    // PA; shared device code never treats a CPU pointer as a physical address.
    let result = unsafe {
        mac_boot_screen::ramfb::configure(super::memory::FW_CFG_BASE as usize,
                                          address,
                                          width(),
                                          height(),
                                          |pointer, bytes| {
                                              super::paging::bus_address(pointer as u64,
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
                super::memory::DISPLAY_BACKBUFFER_BASE,
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
    let attached = video_attached();
    let mut damage = DamageRegion::EMPTY;
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
                if attached
                {
                    damage.add(rect);
                }
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
            PresentEvent::FeedbackDamage(rect) =>
            {
                if attached
                {
                    damage.add(rect);
                }
            }
            PresentEvent::OutlineChanged(_) => ()
        }
    });
    unsafe {
        asm!("fence iorw, iorw", options(nostack));
    }
    restore_dirty(damage);
}
