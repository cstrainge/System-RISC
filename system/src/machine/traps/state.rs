//! Owned CPU state, independent of the application's execution stack.
//! Only the native entry/restore boundary consumes these raw frame pointers.
use super::TrapFrame;
use crate::machine::pages::{Backing, Error};

pub(crate) struct SavedState
{
    backing: Backing,
    frame:   usize,
    bytes:   usize
}
impl SavedState
{
    pub(crate) fn new(initial: TrapFrame) -> Result<Self, Error>
    {
        let bytes = *super::CPU_STATE_BYTES.get().ok_or(Error::Unavailable)?;
        let vector_bytes = bytes.checked_sub(size_of::<TrapFrame>()).ok_or(Error::Geometry)?;
        let backing = Backing::allocate(bytes)?;
        let frame = backing.cpu_base().checked_add(vector_bytes).ok_or(Error::Geometry)?;
        if !frame.is_multiple_of(align_of::<TrapFrame>())
        {
            return Err(Error::Geometry);
        }
        // SAFETY: exclusively owned, zeroed page backing; the checked CPU
        // layout leaves a complete, aligned fixed header after all vector
        // bytes. Neither pointer nor a typed reference has been published.
        unsafe {
            (frame as *mut TrapFrame).write(initial);
        }
        Ok(Self { backing,
                  frame,
                  bytes })
    }

    pub(crate) fn frame(&self) -> *mut TrapFrame
    {
        self.frame as *mut TrapFrame
    }

    pub(crate) fn base(&self) -> usize
    {
        self.backing.cpu_base()
    }

    pub(crate) fn bytes(&self) -> usize
    {
        self.bytes
    }

    pub(crate) fn backing_bytes(&self) -> u64
    {
        self.backing.bytes()
    }

    /// # Safety
    /// Source is the complete current-hart entry frame, with its vector bytes
    /// immediately below it. Its interrupted stack stays pinned and interrupts
    /// stay masked through the copy. The caller owns the selected thread's
    /// state lock; no CPU may restore this destination concurrently.
    pub(crate) unsafe fn capture(&mut self, source: *const TrapFrame)
    {
        if source as usize == self.frame
        {
            return;
        }
        let vector_bytes = self.bytes - size_of::<TrapFrame>();
        // SAFETY: the entry saves exactly this checked CPU layout; source and
        // destination have separate owned leases. Raw copying preserves the
        // complete register state without creating typed vector references.
        unsafe {
            core::ptr::copy_nonoverlapping(source.cast::<u8>().sub(vector_bytes),
                                           self.backing.cpu_base() as *mut u8,
                                           self.bytes);
        }
    }
}
