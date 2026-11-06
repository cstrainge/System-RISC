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

use crate::toolbox::quickdraw::{Rect, geometry::union_rect};

/// The damage accumulated but not yet handed to the driver that presents it.
///
/// Sparse, not one bounding box: a caret-sized change should cost a caret-sized
/// copy in the driver that presents it, and a single box would make every small
/// change copy the whole mode. The merge rules are the display interface's, so
/// this machine accumulates damage exactly as every other one does.
///
/// It accumulates only while a framebuffer is live. On this platform none is yet,
/// so the region stays empty and the compositor presents nothing — the honest
/// outcome, rather than recording damage for a scan-out that does not exist.
pub(crate) struct DamageRegion
{
    rects: [Rect; mac_video_interface::Damage::MAX_RECTS],
    len:   usize
}

impl DamageRegion
{
    pub(crate) const EMPTY: Self = Self { rects: [Rect::ZERO;
                                                  mac_video_interface::Damage::MAX_RECTS],
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
            // Bound the backlog if the display falls behind a complex scene. Keep
            // all pending pixels covered, without forcing a mode-wide update.
            rect = self.rects().iter().copied().fold(rect, union_rect);
            self.len = 0;
        }
        self.rects[self.len] = rect;
        self.len += 1;
    }

    pub(crate) fn rects(&self) -> &[Rect]
    {
        &self.rects[..self.len]
    }
}

static DIRTY: spinning_top::Spinlock<DamageRegion> =
    spinning_top::Spinlock::new(DamageRegion::EMPTY);

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

impl core::fmt::Display for TerminalError
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Unavailable =>
            {
                f.write_str("no framebuffer is available on this platform")
            }
        }
    }
}

/// No terminal surface can be prepared, because there is no framebuffer.
///
/// # Safety
/// Provided for signature parity with the other architecture; it touches nothing.
pub(crate) unsafe fn prepare_terminal(_registers: usize, _framebuffer: u64, _width: usize,
                                      _height: usize,
                                      _translate: impl FnMut(*const u8, usize) -> Option<u64>)
                                      -> Result<PreparedTerminal, TerminalError>
{
    Err(TerminalError::Unavailable)
}

impl PreparedTerminal
{
    /// Adopts the prepared surface. Nothing was prepared, so there is nothing to
    /// adopt; the caller takes its no-display path.
    ///
    /// # Safety
    /// The other architecture requires exclusive ownership of the scanout while
    /// this runs. This implementation touches nothing, so it holds trivially.
    pub(crate) unsafe fn activate(&self) -> Result<(), TerminalError>
    {
        Err(TerminalError::Unavailable)
    }
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

/// Records damage for the next present.
///
/// The damage is accumulated whether or not a front buffer exists, because it is
/// the compositor and the VideoCore framebuffer driver that consume it, not the
/// front-buffer update: the driver copies the damaged rows into the buffer the
/// firmware owns, so a rect this module dropped would be a row the display never
/// received. Sparse rather than one bounding box, so a small change does not make
/// the driver copy the whole mode.
pub(crate) fn mark_dirty(rect: Rect)
{
    super::interrupts::with_masked(|| {
        if video_attached()
        {
            DIRTY.lock().add(rect);
        }
    });
}

pub(crate) fn restore_dirty(region: DamageRegion)
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

pub(crate) fn peek_dirty() -> bool
{
    super::interrupts::with_masked(|| DIRTY.lock().len != 0)
}

pub(crate) fn take_dirty() -> DamageRegion
{
    super::interrupts::with_masked(|| {
        if !video_attached()
        {
            return DamageRegion::EMPTY;
        }
        let mut dirty = DIRTY.lock();
        if dirty.len == 0
        {
            return DamageRegion::EMPTY;
        }
        core::mem::replace(&mut *dirty, DamageRegion::EMPTY)
    })
}

/// Puts back damage that a caller could not paint. Nothing accumulates here
/// for the same reason `mark_dirty` records nothing: there is no front buffer, so
/// restoring damage would be restoring a record this machine never kept.
pub(crate) fn restore_dirty(_region: DamageRegion)
{
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



