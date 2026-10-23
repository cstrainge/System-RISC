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
    }
    ready
}

pub fn ticks() -> u64
{
    unsafe { read_volatile(MTIME as *const u64) }
}
pub fn present(back: &[u32])
{
    present_rects(back, [mac_abi::Rect::new(0, 0, WIDTH as i16, HEIGHT as i16)]);
}

/// Publish only damaged screen rectangles. The compositor keeps a cursor-free
/// backbuffer and supplies the previous/new cursor bounds along with scene damage.
pub fn present_rects(back: &[u32], rects: impl IntoIterator<Item = mac_abi::Rect>)
{
    assert_eq!(back.len(), PIXELS);
    let front = addr_of_mut!(FRAMEBUFFER) as *mut u32;
    for rect in rects
    {
        let left = rect.left.clamp(0, WIDTH as i16) as usize;
        let right = rect.right.clamp(0, WIDTH as i16) as usize;
        let top = rect.top.clamp(0, HEIGHT as i16) as usize;
        let bottom = rect.bottom.clamp(0, HEIGHT as i16) as usize;
        if left >= right || top >= bottom
        {
            continue;
        }
        let started = crate::trace::clock();
        for y in top..bottom
        {
            let at = y * WIDTH + left;
            // RAMFB is ordinary guest RAM, exclusively written by the serialized
            // compositor. Clamped row slices stay within both distinct buffers.
            unsafe {
                core::ptr::copy_nonoverlapping(back.as_ptr().add(at), front.add(at), right - left);
            }
        }
        crate::trace::record(crate::trace::FRAME_COPY,
                             crate::context::current_thread_context().identity().app.0,
                             crate::trace::clock().wrapping_sub(started),
                             ((right - left) * (bottom - top)) as u64,
                             ((left as u64) << 32) | top as u64,
                             ((right as u64) << 32) | bottom as u64);
    }
    unsafe {
        asm!("fence iorw, iorw", options(nostack));
    }
}
