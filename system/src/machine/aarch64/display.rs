//! AArch64 display adapters.
//!
//! This platform's framebuffer is the VideoCore mailbox, which is not implemented
//! yet. Rather than pretend otherwise, every entry point reports that no display
//! is available, so callers take their documented no-display path: the boot
//! splash is skipped and the emergency terminal reports through serial instead of
//! painting. A machine with no working display still has to boot and still has to
//! be able to say what went wrong.
//!
//! When the mailbox framebuffer driver lands, it implements this same surface and
//! nothing above this module changes.

/// A prepared terminal surface. This architecture has none until the mailbox
/// framebuffer exists.
pub(crate) struct PreparedTerminal
{
    bytes: usize
}

impl PreparedTerminal
{
    /// Storage this backend needs in the pinned image. Zero: nothing is prepared.
    pub(crate) fn storage_layout() -> StorageLayout
    {
        StorageLayout { bytes: 0 }
    }

    pub(crate) fn bytes(&self) -> usize
    {
        self.bytes
    }
}

pub(crate) struct StorageLayout
{
    bytes: usize
}

impl StorageLayout
{
    pub(crate) fn size(&self) -> usize
    {
        self.bytes
    }
}

#[derive(Debug)]
pub(crate) enum TerminalError
{
    /// No framebuffer is available on this platform yet.
    Unavailable
}

/// No terminal surface can be prepared, because there is no framebuffer.
///
/// # Safety
/// Provided for signature parity with the other architecture; it touches nothing.
pub(crate) unsafe fn prepare_terminal(_firmware_base: usize, _physical: u64, _width: u16,
                                      _height: u16, _translate: impl Fn(u64, u64) -> Option<u64>)
                                      -> Result<PreparedTerminal, TerminalError>
{
    Err(TerminalError::Unavailable)
}

/// Monotonic time in platform ticks. The scheduler and the file-watch paths
/// measure elapsed intervals with this, so it is a duration and never a date.
pub(crate) fn ticks() -> u64
{
    super::cpu::ticks()
}

/// The display extent. Zero until a framebuffer exists, which is what callers
/// use to decide there is nothing to draw on.
pub(crate) fn width() -> u16
{
    0
}

pub(crate) fn height() -> u16
{
    0
}

/// Whether a framebuffer is live. Nothing can be drawn on this platform yet.
pub(crate) fn video_attached() -> bool
{
    false
}

/// The compositor's back buffer. This platform has no framebuffer, so there is
/// no buffer to hand out; callers reach this only after checking that video is
/// attached, and reaching it otherwise is a defect the empty slice exposes
/// rather than hides.
pub(crate) fn back_buffer() -> &'static mut [u32]
{
    &mut []
}

/// Acquires the framebuffer. There is none to acquire until the mailbox driver
/// exists, so this reports failure rather than claiming success.
pub(crate) fn setup_framebuffer() -> bool
{
    false
}

/// Presents the whole frame. Nothing is displayed, so nothing is copied.
pub(crate) fn present(_back: &[u32])
{
}

/// Presents the damaged rectangles and any overlays. With no framebuffer there is
/// nothing to write, and the empty back buffer means the compositor never gets
/// here with real pixels.
pub(crate) fn present_rects(_back: &[u32],
                            _rects: impl IntoIterator<Item = mac_abi::geometry::Rect>,
                            _overlays: crate::toolbox::quickdraw::presentation::Overlays<'_>)
{
}

/// Records damage for the next present. There is no front buffer to update, so
/// nothing accumulates: the compositor's own bookkeeping is the only consumer.
pub(crate) fn mark_dirty(_rect: mac_abi::geometry::Rect)
{
}

pub(crate) fn peek_dirty() -> bool
{
    false
}

pub(crate) fn take_dirty() -> Option<mac_abi::geometry::Rect>
{
    None
}

/// Adopts a driver-owned framebuffer. There is none until the mailbox driver
/// exists, so this reports that nothing changed rather than claiming adoption.
pub(crate) fn use_driver_front(_address: usize)
{
}

pub(crate) fn copy_front_to(_address: usize)
{
}

pub(crate) fn use_boot_front()
{
}

pub(crate) fn clear_mode_buffers()
{
}



