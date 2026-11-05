//! Scan-out geometry, the copy plan, and the two seams the engine needs from a
//! machine: the firmware mailbox and the cache.
//!
//! The firmware owns the scan-out address. QEMU's `bcm2835_fb` proves it for the
//! property channel: every reconfigure writes `newconf.base = vcram_base +
//! BCM2835_FB_OFFSET` unconditionally, so a request cannot choose where the
//! display fetches from. The surface System draws into is therefore module-owned
//! Device DMA storage, and presenting means copying the damaged rows into the
//! firmware buffer and cleaning them to the point of coherency. That copy is the
//! honest cost of the property channel; a plane-based HVS path removes it, and
//! that hardware is not modelled here.
use mac_video_interface::{Damage, Size};

/// The drawing surface System hands this driver is XRGB32, so the mode's depth
/// and channel order are fixed rather than selectable.
pub const DEPTH: u32 = 32;
/// Requested framebuffer alignment, in bytes. The property channel's request
/// value is an alignment, not a size.
pub const ALLOCATION_ALIGNMENT: u32 = 4096;

/// The modes this driver may apply, from the platform record's display entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds
{
    /// The mode the record starts in.
    pub initial: Size,
    /// The largest mode the record permits.
    pub maximum: Size
}
impl Bounds
{
    /// Smallest permitted side. A zero side is not a mode; the record's own
    /// validation already rejects one for the initial mode.
    pub const MINIMUM_SIDE: u32 = 1;

    pub fn check(self, size: Size) -> Result<(), crate::Refusal>
    {
        if size.width < Self::MINIMUM_SIDE || size.height < Self::MINIMUM_SIDE
        {
            return Err(crate::Refusal::BelowDisplayMinimum { width:  size.width,
                                                             height: size.height });
        }
        if size.width > self.maximum.width
        {
            return Err(crate::Refusal::ExceedsDisplayMaximum { width:  size.width,
                                                               height: size.height,
                                                               field:  "max_width",
                                                               limit:  self.maximum.width });
        }
        if size.height > self.maximum.height
        {
            return Err(crate::Refusal::ExceedsDisplayMaximum { width:  size.width,
                                                               height: size.height,
                                                               field:  "max_height",
                                                               limit:  self.maximum.height });
        }
        Ok(())
    }
}

/// The framebuffer the firmware allocated, as the ARM must use it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame
{
    /// ARM physical base. The library has already reduced the reply's bus
    /// address with 0x3FFFFFFF.
    pub physical: u32,
    /// Bytes the firmware allocated for this mode.
    pub size:     u32,
    /// Bytes per row for *this* mode. It is not carried across modes: a mode
    /// change has its own pitch, and drawing with the previous one shears the
    /// image.
    pub pitch:    u32,
    pub geometry: Size
}

/// Module-owned, bus-addressable storage for the surface System draws into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Allocation
{
    pub handle:  u64,
    /// CPU address System draws at. It is also what `Surface::address` reports.
    pub address: u64,
    pub length:  u64
}

/// One damage rectangle as row ranges: what to copy, from where, to where, and
/// what to clean afterwards. Pure arithmetic, so the ranges can be checked
/// without a device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan
{
    pub rows:              usize,
    /// Words per damaged row.
    pub row_words:         usize,
    /// Offset into the drawing surface, in words. The surface is tightly packed,
    /// so its stride is the mode width.
    pub source_words:      usize,
    /// Offset into the firmware buffer, in bytes, using the current pitch.
    pub destination_bytes: usize,
    /// Bytes to clean, starting at `destination_bytes`. The range spans the
    /// damaged rows and the padding between them, which is bounded by the
    /// rectangle and keeps one call per present.
    pub clean_bytes:       usize
}

/// The rows of `damage` inside `geometry`, positioned with `pitch`.
///
/// `None` when the rectangle is empty or entirely outside the mode; a caller
/// then presents nothing rather than clamping a bogus rectangle into view.
pub fn plan(geometry: Size, pitch: u32, damage: Damage) -> Option<Plan>
{
    let right = damage.x.checked_add(damage.width)?.min(geometry.width);
    let bottom = damage.y.checked_add(damage.height)?.min(geometry.height);
    if damage.x >= right || damage.y >= bottom
    {
        return None;
    }
    let rows = (bottom - damage.y) as usize;
    let row_words = (right - damage.x) as usize;
    let row_bytes = row_words.checked_mul(4)?;
    let destination_bytes = (damage.y as usize).checked_mul(pitch as usize)?
                                               .checked_add((damage.x as usize).checked_mul(4)?)?;
    let clean_bytes = (rows - 1).checked_mul(pitch as usize)?.checked_add(row_bytes)?;
    Some(Plan { rows,
                row_words,
                source_words: (damage.y as usize).checked_mul(geometry.width as usize)?
                                                 .checked_add(damage.x as usize)?,
                destination_bytes,
                clean_bytes })
}

/// What the engine needs from the machine. The host tests implement this over a
/// simulated mailbox; the module implements it over MMIO, the Device DMA
/// service and the cache instructions.
pub trait Platform
{
    /// Ask the firmware for a framebuffer at `geometry`, replacing whatever it
    /// was scanning out. The property channel has no staged mode: this applies
    /// the mode immediately.
    fn acquire(&mut self, geometry: Size, alignment: u32) -> Result<Frame, crate::Refusal>;

    /// Allocate module-owned storage for the drawing surface, charged to this
    /// module by the Device DMA service.
    fn allocate(&mut self, bytes: u64) -> Result<Allocation, crate::Refusal>;

    /// Return that storage once System has retired its CPU pin.
    fn free(&mut self, allocation: Allocation) -> Result<(), crate::Refusal>;

    /// Copy the planned rows from the drawing surface into the firmware buffer.
    fn blit(&mut self, allocation: Allocation, frame: &Frame, plan: &Plan)
            -> Result<(), crate::Refusal>;

    /// Clean `bytes` of the scan-out at `offset` to the point of coherency.
    ///
    /// This is the cache capability: the machine adapter issues the
    /// architecture's maintenance, and a host build records the range instead.
    /// It is not optional. A linear framebuffer is exactly where a cached write
    /// can sit unseen by the display engine, and a no-op here would only look
    /// correct on an emulator that models no caches.
    fn clean(&mut self, frame: &Frame, offset: usize, bytes: usize);

    /// A monotonic microsecond reading from the system's clock service.
    fn now(&mut self) -> Result<u64, crate::Refusal>;
}
