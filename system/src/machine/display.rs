//! QEMU virt display and clock; System owns the framebuffers.
use core::{arch::asm,
           ptr::{addr_of_mut, read_volatile},
           sync::atomic::{AtomicBool, Ordering}};

/// Display size in pixels, from the build-time platform record
/// (`platforms/<platform>.json`, `memory.display`). This is the single source
/// of display geometry: every consumer reads these constants.
/// Control Panel hook: a future Display Control Panel changes the size by
/// backing these with a runtime preference instead of the platform record;
/// runtime switching also requires dynamic framebuffers, caches and scratch,
/// not just this value.
pub(crate) use crate::display::{HEIGHT, PIXELS, WIDTH};
const MTIME: usize = 0x0200bff8;
#[repr(C, align(4096))]
struct Pixels([u32; PIXELS]);
static mut BACKBUFFER: Pixels = Pixels([0; PIXELS]);
// Window tracking never paints the menu bar's first 21 rows.
const TRACKING_PIXELS: usize = WIDTH * (HEIGHT - 21);
// This physical scratch reservation is generated from the same checked
// platform record as the loader arena and BootInfo reservations. It is not
// image BSS and cannot be allocated by the later physical page pool.
const _: () =
    assert!(super::memory::DISPLAY_TRACKING_BASE + (TRACKING_PIXELS * size_of::<u32>()) as u64
            <= super::memory::DISPLAY_SCRATCH_BASE + super::memory::DISPLAY_SCRATCH_BYTES);
static TRACKING_INITIALIZED: AtomicBool = AtomicBool::new(false);
static TRACKING_AVAILABLE: AtomicBool = AtomicBool::new(false);
static TRACKING_BORROWED: AtomicBool = AtomicBool::new(false);

/// Scratch belongs to the serialized compositor and cannot escape this call.
/// A future/reentrant caller receives None instead of aliasing the active mask.
pub fn with_tracking_mask<R>(call: impl FnOnce(Option<&mut [u32]>) -> R) -> R
{
    // Bootstrap still owns the shared range until our framebuffer is installed.
    if !TRACKING_AVAILABLE.load(Ordering::Acquire)
    {
        return call(None);
    }
    if TRACKING_BORROWED.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                        .is_err()
    {
        return call(None);
    }
    struct Borrow;
    impl Drop for Borrow
    {
        fn drop(&mut self)
        {
            TRACKING_BORROWED.store(false, Ordering::Release);
        }
    }
    let _borrow = Borrow;
    let pointer = super::memory::DISPLAY_TRACKING_BASE as *mut u32;
    // Only this serialized device boundary accesses the dedicated RAM range.
    // Initialize bytes before publishing a reference, once per boot; subsequent
    // frames reuse the same pinned storage without allocation or clearing here.
    if !TRACKING_INITIALIZED.load(Ordering::Acquire)
    {
        unsafe {
            core::ptr::write_bytes(pointer, 0, TRACKING_PIXELS);
        }
        TRACKING_INITIALIZED.store(true, Ordering::Release);
    }
    // The platform checker proves page alignment, non-overlap and containment
    // in discovered minimum RAM; the guarded slice cannot escape this call.
    call(Some(unsafe { core::slice::from_raw_parts_mut(pointer, TRACKING_PIXELS) }))
}

pub fn back_buffer() -> &'static mut [u32]
{
    // Single hart, only the System compositor uses this buffer.
    unsafe { core::slice::from_raw_parts_mut(addr_of_mut!(BACKBUFFER).cast::<u32>(), PIXELS) }
}
pub fn setup_framebuffer() -> bool
{
    let address = super::memory::DISPLAY_FRONTBUFFER_BASE;
    // The checked platform frontbuffer reservation is pinned for System's lifetime; bootstrap owns
    // exclusive fw_cfg access, before any application or secondary hart starts.
    let ready = unsafe { mac_boot_screen::ramfb::configure(address, WIDTH, HEIGHT) };
    if ready
    {
        TRACKING_AVAILABLE.store(true, Ordering::Release);
        serial!("RAMFB {}x{} addr=0x{:x}", WIDTH, HEIGHT, address);
        serial!("DISPLAY_BUFFERS back={} front={} bytes={}",
                addr_of_mut!(BACKBUFFER) as usize,
                address,
                PIXELS * size_of::<u32>());
    }
    ready
}

pub fn ticks() -> u64
{
    unsafe { read_volatile(MTIME as *const u64) }
}
pub fn present(back: &[u32])
{
    present_rects(back,
                  [mac_abi::Rect::new(0, 0, WIDTH as i16, HEIGHT as i16)],
                  Default::default());
}

/// Copy real scene damage and apply frontbuffer-only cursor/drag transitions.
pub(crate) fn present_rects(back: &[u32], rects: impl IntoIterator<Item = mac_abi::Rect>,
                            overlays: crate::toolbox::quickdraw::presentation::Overlays<'_>)
{
    use crate::toolbox::quickdraw::presentation::{PresentEvent, present_frame};
    // The serialized compositor owns both buffers. This exclusive frontbuffer
    // slice cannot escape; the scene stays read-only for the entire transition.
    let front = unsafe {
        core::slice::from_raw_parts_mut(super::memory::DISPLAY_FRONTBUFFER_BASE as *mut u32, PIXELS)
    };
    let mut started = 0;
    let owner = crate::context::current_thread_context().identity().app.0;
    present_frame(back, front, WIDTH, HEIGHT, rects, overlays, |event| {
        match event
        {
            PresentEvent::CopyStarted | PresentEvent::OutlineStarted =>
            {
                started = crate::trace::clock()
            }
            PresentEvent::Copied(rect) =>
            {
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
