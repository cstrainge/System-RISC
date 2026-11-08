//! Owns the stack mapping for a same-thread Shelly coroutine.
use std::{io, marker::PhantomData, num::NonZeroUsize, rc::Rc};

use corosensei::stack::{Stack, StackPointer};
use mac_toolbox::{System, abi::coroutine_stack as wire};

pub(super) struct TaskStack
{
    base:  usize,
    top:   StackPointer,
    limit: StackPointer,
    _thread: PhantomData<Rc<()>>
}

impl TaskStack
{
    pub(super) fn new(bytes: usize) -> io::Result<Self>
    {
        let system = System::new().map_err(io::Error::other)?;
        let mut call = wire::Call { operation: wire::ALLOCATE,
                                   bytes: bytes as u64,
                                   ..Default::default() };
        // SAFETY: allocate has no live mapping input.
        unsafe { system.client().coroutine_stack(&mut call) }.map_err(io::Error::other)?;
        let top = NonZeroUsize::new(call.top).ok_or(io::ErrorKind::InvalidData)?;
        let limit = NonZeroUsize::new(call.limit).ok_or(io::ErrorKind::InvalidData)?;
        Ok(Self { base: call.base,
                  top,
                  limit,
                  _thread: PhantomData })
    }
}

// SAFETY: the resident mapping owns writable, page-aligned memory above an
// inaccessible guard. The mapping stays live for this thread-affine owner.
unsafe impl Stack for TaskStack
{
    fn base(&self) -> StackPointer { self.top }

    fn limit(&self) -> StackPointer { self.limit }
}

impl Drop for TaskStack
{
    fn drop(&mut self)
    {
        if let Ok(system) = System::new()
        {
            let mut call = wire::Call { operation: wire::RELEASE,
                                       base: self.base,
                                       top: self.top.get(),
                                       ..Default::default() };
            // SAFETY: corosensei drops the stack only after the coroutine no
            // longer has live frames. A refused mapping stays thread-owned.
            if let Err(error) = unsafe { system.client().coroutine_stack(&mut call) }
            {
                let _ = system.log(&format!("Shelly coroutine stack release refused: {error:?}\n"));
            }
        }
    }
}
