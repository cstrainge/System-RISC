//! Owned CPU state, independent of the application's execution stack.
//! Only the native entry/restore boundary consumes these raw frame pointers.
use super::TrapFrame;
use crate::machine::pages::{Backing, Error};

pub(crate) struct SavedState
{
    backing: Backing,
    frame:   usize,
    bytes:   usize,
    stride:  usize
}
impl SavedState
{
    pub(crate) fn new(initial: TrapFrame) -> Result<Self, Error>
    {
        let stride = *super::CPU_STATE_BYTES.get().ok_or(Error::Unavailable)?;
        let bytes = stride.checked_mul(super::CPU_SLOTS).ok_or(Error::Geometry)?;
        let vector_bytes = stride.checked_sub(size_of::<TrapFrame>()).ok_or(Error::Geometry)?;
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
                  bytes,
                  stride })
    }

    pub(crate) fn frame(&self) -> *mut TrapFrame
    {
        self.frame as *mut TrapFrame
    }

    pub(crate) fn base(&self) -> usize
    {
        self.backing.cpu_base()
    }

    pub(crate) fn physical(&self) -> u64
    {
        self.backing.physical().0
    }

    pub(crate) fn stride(&self) -> usize
    {
        self.stride
    }

    pub(crate) fn bytes(&self) -> usize
    {
        self.bytes
    }

    pub(crate) fn backing_bytes(&self) -> u64
    {
        self.backing.bytes()
    }
}
