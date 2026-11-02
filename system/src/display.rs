//! Checked build-platform geometry shared by resident services. The platform
//! fixes the default and maximum sizes; the live size changes only through
//! `set_mode`, which refuses sizes outside the checked range.
#![forbid(unsafe_code)]
use core::sync::atomic::{AtomicU32, Ordering};

include!(concat!(env!("OUT_DIR"), "/platform-display.rs"));
/// Smallest mode a driver may apply; keeps the menu bar and Finder usable.
pub(crate) const MIN_WIDTH: usize = 640;
pub(crate) const MIN_HEIGHT: usize = 480;
pub(crate) const MAX_PIXELS: usize = MAX_WIDTH * MAX_HEIGHT;
static MODE: AtomicU32 = AtomicU32::new(((DEFAULT_WIDTH as u32) << 16) | DEFAULT_HEIGHT as u32);
/// Typed refusal of a requested mode; the previous mode stays in force.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModeRefusal
{
    TooSmall,
    TooLarge
}
pub(crate) fn width() -> usize
{
    (MODE.load(Ordering::Acquire) >> 16) as usize
}
pub(crate) fn height() -> usize
{
    (MODE.load(Ordering::Acquire) & 0xffff) as usize
}
pub(crate) fn pixels() -> usize
{
    let mode = MODE.load(Ordering::Acquire);
    (mode >> 16) as usize * (mode & 0xffff) as usize
}
/// Checks a candidate mode without applying it.
pub(crate) fn validate(width: usize, height: usize) -> Result<(), ModeRefusal>
{
    if width < MIN_WIDTH || height < MIN_HEIGHT
    {
        Err(ModeRefusal::TooSmall)
    }
    else if width > MAX_WIDTH || height > MAX_HEIGHT
    {
        Err(ModeRefusal::TooLarge)
    }
    else
    {
        Ok(())
    }
}
/// Publishes the live size. Callers own re-zeroing the display buffers.
pub(crate) fn set_mode(width: usize, height: usize) -> Result<(), ModeRefusal>
{
    validate(width, height)?;
    MODE.store(((width as u32) << 16) | height as u32, Ordering::Release);
    Ok(())
}
