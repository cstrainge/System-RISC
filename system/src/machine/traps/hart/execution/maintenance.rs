//! Logical System service execution borrowed only during a complete captured
//! scheduler turn. The interrupted CPU frame, root and running claim stay owned.
#![forbid(unsafe_code)]
use super::*;

/// This is a logical invocation reader, never a scheduled-idle claim. Holding
/// the native capture prevents the caller from consuming it for frame selection
/// until the override is gone. The canonical idle Context stays pinned even
/// while nested firmware temporarily selects its existing bootstrap identity.
struct Entered<'a>
{
    local:    &'static HartLocal,
    _capture: &'a crate::machine::traps::OuterCapture,
    _context: Arc<ThreadContext>,
    _affine:  core::marker::PhantomData<*mut ()>
}
impl Drop for Entered<'_>
{
    fn drop(&mut self)
    {
        with_masked(|| {
            if !core::ptr::eq(required(), self.local)
               || !self.local.maintenance.swap(false, Ordering::Relaxed)
            {
                crate::machine::traps::entry_lost("captured System invocation lost its same-hart \
                                                   restoration binding");
            }
        });
        // The owned Context pin drops after restoration and outside the gate.
    }
}

/// Called after scheduler validate_capture and before changing any physical
/// selection. No scope caller can nominate a Context, owner, role or thread ID.
/// Only short metadata transitions are masked; operation retains entry's SIE
/// state and must remain synchronous on the original System stack.
pub(crate) fn with_system_maintenance<R>(capture: &crate::machine::traps::OuterCapture,
                                                     operation: impl FnOnce() -> R)
                                                     -> R
{
    let entered = with_masked(|| {
        let local = required();
        if capture.hart() != selected_hart()
           || capture.current() != current()
           || local.io.load(Ordering::Relaxed)
           || local.maintenance.load(Ordering::Relaxed)
        {
            crate::machine::traps::entry_lost("System invocation requires its original same-hart \
                                               outer capture without a nested override");
        }
        let context = {
            let idle = local.idle.lock();
            let Some(idle) = idle.as_ref()
            else
            {
                crate::machine::traps::entry_lost("System invocation has no canonical affine \
                                                   idle Context");
            };
            if !core::ptr::eq(idle.target().local, local)
            {
                crate::machine::traps::entry_lost("System invocation idle Context belongs to \
                                                   another hart");
            }
            idle.context().clone()
        };
        local.maintenance.store(true, Ordering::Relaxed);
        Entered { local,
                  _capture: capture,
                  _context: context,
                  _affine: core::marker::PhantomData }
    });
    let result = operation();
    drop(entered);
    result
}
