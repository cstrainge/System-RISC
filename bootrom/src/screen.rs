//! ROM owns one pinned framebuffer until System reconfigures RAMFB. Only the
//! boot hart enters here; callbacks retained by System never touch this storage.
use mac_boot_screen::Raster;

use crate::memory::{DISPLAY_HEIGHT, DISPLAY_SCRATCH_BASE, DISPLAY_SCRATCH_BYTES, DISPLAY_WIDTH};
const WIDTH: usize = DISPLAY_WIDTH as usize;
const HEIGHT: usize = DISPLAY_HEIGHT as usize;
// The checked platform range contains boot/front pixels followed by separate
// resident tracking scratch. System retains the frontbuffer after handoff;
// firmware storage callbacks never access display memory afterward.
const _: () = assert!(DISPLAY_SCRATCH_BYTES >= (WIDTH * HEIGHT * size_of::<u32>()) as u64);
fn raster() -> Option<Raster<'static>>
{
    // Serialized boot-only access. No raster escapes this private module.
    let pixels = unsafe {
        core::slice::from_raw_parts_mut(DISPLAY_SCRATCH_BASE as *mut u32, WIDTH * HEIGHT)
    };
    Raster::new(pixels, WIDTH, HEIGHT)
}
pub fn init() -> bool
{
    // Initialize physical bytes before publishing any reference. The boot hart
    // exclusively owns this range until the successful System RAMFB handoff.
    unsafe {
        core::ptr::write_bytes(DISPLAY_SCRATCH_BASE as *mut u32, 0, WIDTH * HEIGHT);
    }
    let Some(mut canvas) = raster()
    else
    {
        return false;
    };
    canvas.background();
    // Static pinned RAM remains valid after the ROM hands off to System.
    unsafe { mac_boot_screen::ramfb::configure(DISPLAY_SCRATCH_BASE, WIDTH, HEIGHT) }
}
pub fn happy()
{
    if let Some(mut canvas) = raster()
    {
        canvas.icon(mac_boot_screen::HAPPY, (WIDTH as i16 - 32) / 2, (HEIGHT as i16 - 32) / 2, 1);
    }
}
pub fn missing()
{
    if let Some(mut canvas) = raster()
    {
        canvas.icon(mac_boot_screen::MISSING, (WIDTH as i16 - 32) / 2, (HEIGHT as i16 - 32) / 2, 1);
    }
}
