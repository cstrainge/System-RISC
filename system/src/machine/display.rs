//! QEMU virt display and clock; System owns the framebuffers.
use core::{arch::asm,
           ptr::{addr_of_mut, read_volatile},
           sync::atomic::{AtomicBool, Ordering}};
pub const WIDTH: usize = 640;
pub const HEIGHT: usize = 480;
pub const PIXELS: usize = WIDTH * HEIGHT;
const MTIME: usize = 0x0200bff8;
#[repr(C, align(4096))]
struct Pixels([u32; PIXELS]);
static mut FRAMEBUFFER: Pixels = Pixels([0; PIXELS]);
static mut BACKBUFFER: Pixels = Pixels([0; PIXELS]);
// Window tracking never paints the menu bar's first 21 rows.
const TRACKING_PIXELS: usize = WIDTH * (HEIGHT - 21);
#[repr(C, align(4096))]
struct TrackingPixels([u32; TRACKING_PIXELS]);
static mut TRACKING_MASK: TrackingPixels = TrackingPixels([0; TRACKING_PIXELS]);
static TRACKING_BORROWED: AtomicBool = AtomicBool::new(false);

/// Scratch belongs to the serialized compositor and cannot escape this call.
/// A future/reentrant caller receives None instead of aliasing the active mask.
pub fn with_tracking_mask<R>(call: impl FnOnce(Option<&mut [u32]>) -> R) -> R
{
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
    // The guard excludes all other borrows until the closure returns, the
    // storage is initialized/aligned, and its lifetime cannot escape R.
    call(Some(unsafe {
             core::slice::from_raw_parts_mut(addr_of_mut!(TRACKING_MASK).cast::<u32>(),
                                             TRACKING_PIXELS)
         }))
}

pub fn back_buffer() -> &'static mut [u32]
{
    // Single hart, only the System compositor uses this buffer.
    unsafe { core::slice::from_raw_parts_mut(addr_of_mut!(BACKBUFFER).cast::<u32>(), PIXELS) }
}
pub fn setup_framebuffer() -> bool
{
    let address = addr_of_mut!(FRAMEBUFFER) as u64;
    // The static framebuffer is pinned for System's lifetime; bootstrap owns
    // exclusive fw_cfg access, before any application or secondary hart starts.
    let ready = unsafe { mac_boot_screen::ramfb::configure(address, WIDTH, HEIGHT) };
    if ready
    {
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
    present_rects(back, [mac_abi::Rect::new(0, 0, WIDTH as i16, HEIGHT as i16)], None);
}

/// Publish only damaged screen rectangles. The compositor keeps a cursor-free
/// backbuffer and supplies the previous/new cursor bounds along with scene damage.
pub(crate) fn present_rects(back: &[u32], rects: impl IntoIterator<Item = mac_abi::Rect>,
                            cursor: Option<crate::toolbox::quickdraw::presentation::CursorSprite<'_>>)
{
    // The serialized compositor owns both buffers. This exclusive frontbuffer
    // slice cannot escape; the scene is borrowed read-only for the entire copy.
    let front =
        unsafe { core::slice::from_raw_parts_mut(addr_of_mut!(FRAMEBUFFER).cast::<u32>(), PIXELS) };
    for rect in rects
    {
        let started = crate::trace::clock();
        let Some(rect) = crate::toolbox::quickdraw::presentation::present_rect(back, front, WIDTH,
                                                                               HEIGHT, rect,
                                                                               cursor)
        else
        {
            continue;
        };
        crate::trace::record(crate::trace::FRAME_COPY,
                             crate::context::current_thread_context().identity().app.0,
                             crate::trace::clock().wrapping_sub(started),
                             (rect.right - rect.left) as u64 * (rect.bottom - rect.top) as u64,
                             ((rect.left as u64) << 32) | rect.top as u64,
                             ((rect.right as u64) << 32) | rect.bottom as u64);
    }
    unsafe {
        asm!("fence iorw, iorw", options(nostack));
    }
}
