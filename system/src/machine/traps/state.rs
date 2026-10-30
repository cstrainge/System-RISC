//! Owned CPU state, independent of the application's execution stack.
//! Only the native entry/restore boundary consumes these raw frame pointers.
use super::TrapFrame;
use crate::machine::pages::{Backing, Error, Storage};

pub(crate) struct SavedState
{
    backing: Storage,
    frame:   usize,
    bytes:   usize,
    stride:  usize
}
impl SavedState
{
    fn layout() -> Result<(usize, usize), Error>
    {
        let stride = *super::CPU_STATE_BYTES.get().ok_or(Error::Unavailable)?;
        let bytes = stride.checked_mul(super::CPU_SLOTS).ok_or(Error::Geometry)?;
        Ok((stride, bytes))
    }

    pub(crate) fn frame_bytes() -> Result<u64, Error>
    {
        Ok(Self::layout()?.0 as u64)
    }

    pub(crate) fn planned_backing_bytes() -> Result<u64, Error>
    {
        Backing::planned_bytes(Self::layout()?.1)
    }

    pub(crate) fn new(initial: TrapFrame) -> Result<Self, Error>
    {
        let (stride, bytes) = Self::layout()?;
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
        Ok(Self { backing: Storage::Ordinary(backing),
                  frame,
                  bytes,
                  stride })
    }

    pub(crate) fn from_recovery(mut piece: crate::machine::scheduler::recovery::FinderPiece,
                                initial: TrapFrame)
                                -> Self
    {
        let (stride, bytes) = match Self::layout()
        {
            Ok(layout) => layout,
            Err(_) =>
            {
                crate::machine::emergency::enter(crate::emergency::Kind::SystemPanic,
                                                 format_args!("Finder CPU layout unavailable \
                                                               after boot admission."))
            }
        };
        let vector_bytes = stride - size_of::<TrapFrame>();
        let frame = piece.cpu_base() + vector_bytes;
        if bytes > piece.bytes() as usize || !frame.is_multiple_of(align_of::<TrapFrame>())
        {
            crate::machine::emergency::enter(crate::emergency::Kind::SystemPanic,
                                             format_args!("Finder CPU backing violates \
                                                           boot-admitted layout."));
        }
        piece.activate();
        // SAFETY: unique, fully scrubbed protected piece; layout was admitted at
        // boot and cannot change during this one-hart boot. No frame is published.
        unsafe {
            (frame as *mut TrapFrame).write(initial);
        }
        Self { backing: Storage::Recovery(piece),
               frame,
               bytes,
               stride }
    }

    /// Compare this independently measured CPU against the actual admitted
    /// backing/layout, rather than treating the primary's cached width as an
    /// observation of another CPU. No frame/header borrow escapes.
    pub(super) fn matches_vector_profile(&self, vector_bytes: usize) -> bool
    {
        let Some(stride) =
            vector_bytes.checked_mul(32)
                        .and_then(|vectors| size_of::<TrapFrame>().checked_add(vectors))
        else
        {
            return false;
        };
        stride == self.stride
        && stride.checked_mul(super::CPU_SLOTS) == Some(self.bytes)
        && self.base().checked_add(vector_bytes * 32) == Some(self.frame)
        && self.frame.is_multiple_of(align_of::<TrapFrame>())
        && self.backing.bytes() >= self.bytes as u64
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
