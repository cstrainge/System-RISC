//! QEMU virt display and clock; System owns the framebuffers.
use core::{arch::asm,
           ptr::{addr_of_mut, read_volatile, write_volatile},
           sync::atomic::{AtomicBool, Ordering}};
pub const WIDTH: usize = 640;
pub const HEIGHT: usize = 480;
pub const PIXELS: usize = WIDTH * HEIGHT;
const FW_CFG: usize = 0x10100000;
const MTIME: usize = 0x0200bff8;
#[repr(C, align(4096))]
struct Pixels([u32; PIXELS]);
static mut FRAMEBUFFER: Pixels = Pixels([0; PIXELS]);
static mut BACKBUFFER: Pixels = Pixels([0; PIXELS]);
static mut TRACKING_MASK: Pixels = Pixels([0; PIXELS]);
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
             core::slice::from_raw_parts_mut(addr_of_mut!(TRACKING_MASK).cast::<u32>(), PIXELS)
         }))
}

pub fn back_buffer() -> &'static mut [u32]
{
    // Single hart, only the System compositor uses this buffer.
    unsafe { core::slice::from_raw_parts_mut(addr_of_mut!(BACKBUFFER).cast::<u32>(), PIXELS) }
}
unsafe fn select(selector: u16)
{
    unsafe {
        write_volatile((FW_CFG + 8) as *mut u16, selector.to_be());
    }
}
fn cfg_byte() -> u8
{
    unsafe { read_volatile(FW_CFG as *const u8) }
}
fn cfg_u16() -> u16
{
    u16::from_be_bytes([cfg_byte(), cfg_byte()])
}
fn cfg_u32() -> u32
{
    u32::from_be_bytes([cfg_byte(), cfg_byte(), cfg_byte(), cfg_byte()])
}

fn find_file(name: &[u8]) -> Option<(u16, u32)>
{
    unsafe {
        select(0x19);
    }
    let count = cfg_u32();
    if count > 4096
    {
        return None;
    }
    for _ in 0..count
    {
        let length = cfg_u32();
        let selector = cfg_u16();
        let _reserved = cfg_u16();
        let mut filename = [0u8; 56];
        for byte in &mut filename
        {
            *byte = cfg_byte();
        }
        if filename.split(|b| *b == 0).next() == Some(name)
        {
            return Some((selector, length));
        }
    }
    None
}

#[repr(C, align(8))]
struct Dma
{
    control: u32,
    length:  u32,
    address: u64
}

pub fn setup_framebuffer() -> bool
{
    unsafe {
        select(0);
    }
    assert_eq!([cfg_byte(), cfg_byte(), cfg_byte(), cfg_byte()], *b"QEMU");
    assert_eq!(u64::from_be(unsafe { read_volatile((FW_CFG + 16) as *const u64) }),
               0x5145_4d55_2043_4647);
    let Some((selector, length)) = find_file(b"etc/ramfb")
    else
    {
        return false;
    };
    assert_eq!(length, 28);
    let address = addr_of_mut!(FRAMEBUFFER) as u64;
    // RAMFBCfg is packed, big endian. XR24 = XRGB8888, pixels in little-endian RAM.
    let mut config = [0u8; 28];
    config[0..8].copy_from_slice(&address.to_be_bytes());
    config[8..12].copy_from_slice(&u32::from_le_bytes(*b"XR24").to_be_bytes());
    config[16..20].copy_from_slice(&(WIDTH as u32).to_be_bytes());
    config[20..24].copy_from_slice(&(HEIGHT as u32).to_be_bytes());
    config[24..28].copy_from_slice(&((WIDTH * 4) as u32).to_be_bytes());
    let mut dma = Dma { control: ((u32::from(selector) << 16) | 8 | 16).to_be(),
                        length:  28u32.to_be(),
                        address: (config.as_ptr() as u64).to_be() };
    unsafe {
        asm!("fence iorw, iorw", options(nostack));
        write_volatile((FW_CFG + 16) as *mut u64, (addr_of_mut!(dma) as u64).to_be());
        loop
        {
            let control = u32::from_be(read_volatile(addr_of_mut!(dma.control)));
            assert_eq!(control & 1, 0, "framebuffer DMA failed");
            if control == 0
            {
                break;
            }
        }
        asm!("fence iorw, iorw", options(nostack));
    }
    serial!("RAMFB {}x{} addr=0x{:x}", WIDTH, HEIGHT, address);
    true
}

pub fn ticks() -> u64
{
    unsafe { read_volatile(MTIME as *const u64) }
}
pub fn present(back: &[u32])
{
    assert_eq!(back.len(), PIXELS);
    let started = crate::trace::clock();
    let front = addr_of_mut!(FRAMEBUFFER) as *mut u32;
    // RAMFB is ordinary guest RAM, not a register aperture. The compositor is
    // its sole writer; back and front are distinct, equally sized allocations.
    // Copy in bulk and publish through the compiler/hardware fence below.
    unsafe {
        core::ptr::copy_nonoverlapping(back.as_ptr(), front, PIXELS);
        asm!("fence iorw, iorw", options(nostack));
    }
    crate::trace::record(crate::trace::FRAME_COPY,
                         crate::context::current_thread_context().identity().app.0,
                         crate::trace::clock().wrapping_sub(started),
                         PIXELS as u64,
                         0,
                         0);
}
