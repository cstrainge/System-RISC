//! ROM owns one pinned framebuffer until System reconfigures RAMFB. Only the
//! boot hart enters here; callbacks retained by System never touch this storage.
use core::ptr::addr_of_mut;

use mac_boot_screen::Raster;
const WIDTH: usize = 640;
const HEIGHT: usize = 480;
#[repr(C, align(4096))]
struct Pixels([u32; WIDTH * HEIGHT]);
static mut PIXELS: Pixels = Pixels([0; WIDTH * HEIGHT]);
fn raster() -> Raster<'static>
{
    // Serialized boot-only access. No raster escapes this private module.
    let pixels = unsafe {
        core::slice::from_raw_parts_mut(addr_of_mut!(PIXELS).cast::<u32>(), WIDTH * HEIGHT)
    };
    Raster::new(pixels, WIDTH, HEIGHT).unwrap()
}
pub fn init() -> bool
{
    raster().background();
    // Static pinned RAM remains valid after the ROM hands off to System.
    unsafe { mac_boot_screen::ramfb::configure(addr_of_mut!(PIXELS) as u64, WIDTH, HEIGHT) }
}
pub fn happy()
{
    raster().icon(mac_boot_screen::HAPPY, 304, 224, 1);
}
pub fn missing()
{
    raster().icon(mac_boot_screen::MISSING, 304, 224, 1);
}
