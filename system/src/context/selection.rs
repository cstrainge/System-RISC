//! Thread-owned invocation selections. Owned capabilities and copyable metadata
//! use the same generation-checked restoration. Reset only after the old stack
//! is quiescent; an old restore never changes a reused thread's context. Payload
//! destruction happens outside the slot lock, including reset and restoration.
#![forbid(unsafe_code)]
use mac_object_table::shared::InvocationGate as _;
use spinning_top::Spinlock;

use super::ThreadId;
use crate::toolbox::handles::InvocationGate;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Reclaimed;
impl core::fmt::Display for Reclaimed
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        f.write_str("resource selection has no scheduled thread; its context was reclaimed")
    }
}
struct State<T>
{
    thread:  ThreadId,
    current: Option<T>
}
pub(super) struct ScopedSelection<T: Clone>
{
    state: Spinlock<State<T>>
}
impl<T: Clone> ScopedSelection<T>
{
    pub(super) const fn new(thread: ThreadId) -> Self
    {
        Self { state: Spinlock::new(State { thread,
                                            current: None }) }
    }

    /// Called only when assigning a genuine newly issued thread, or after the
    /// scheduler has proved the old stack is quiescent. Never issues an ID.
    pub(super) fn reset(&self, thread: ThreadId)
    {
        let previous = InvocationGate::run(|| {
            let mut state = self.state.lock();
            state.thread = thread;
            state.current.take()
        });
        drop(previous);
    }

    pub(super) fn current(&self) -> Option<T>
    {
        InvocationGate::run(|| self.state.lock().current.clone())
    }

    /// Borrow only for an allocation-free metadata operation. In particular,
    /// allocator admission must not clone/drop an AccountUse on every request.
    pub(super) fn borrow<R>(&self, operation: impl FnOnce(Option<&T>) -> R) -> R
    {
        InvocationGate::run(|| operation(self.state.lock().current.as_ref()))
    }

    /// The caller holds the actual current context pin until restoration. Slot
    /// replacement cannot fail or mint account authority; reservation still
    /// validates the selected capability. No guard escapes this closure.
    pub(super) fn with_pinned<R>(&self, value: T, operation: impl FnOnce() -> R) -> R
    {
        let (thread, previous) = InvocationGate::run(|| {
            let mut state = self.state.lock();
            (state.thread, state.current.replace(value))
        });
        let _restore = Restore { state: self,
                                 thread,
                                 previous };
        operation()
    }

    pub(super) fn with<R>(&self, value: T, operation: impl FnOnce() -> R) -> Result<R, Reclaimed>
    {
        let (thread, previous) = InvocationGate::run(|| {
            let mut state = self.state.lock();
            if state.thread.0 == 0
            {
                return Err(Reclaimed);
            }
            let thread = state.thread;
            let previous = state.current.replace(value);
            Ok((thread, previous))
        })?;
        let _restore = Restore { state: self,
                                 thread,
                                 previous };
        Ok(operation())
    }
}
/// Bootstrap is one permanent logical OS thread. Its selector cannot be reset
/// or reclaimed, so the architecture handoff needs no fallible fallback or
/// panic on cleanup/paint callers. Reuse the same guarded restore mechanism.
pub(super) struct PermanentSelection<T: Clone>
{
    inner: ScopedSelection<T>
}
impl<T: Clone> PermanentSelection<T>
{
    pub(super) const fn new(thread: ThreadId) -> Self
    {
        Self { inner: ScopedSelection::new(thread) }
    }

    pub(super) fn current(&self) -> Option<T>
    {
        self.inner.current()
    }

    pub(super) fn borrow<R>(&self, operation: impl FnOnce(Option<&T>) -> R) -> R
    {
        self.inner.borrow(operation)
    }

    pub(super) fn with<R>(&self, selected: Option<T>, operation: impl FnOnce() -> R) -> R
    {
        let (thread, previous) = InvocationGate::run(|| {
            let mut state = self.inner.state.lock();
            let thread = state.thread;
            let previous = core::mem::replace(&mut state.current, selected);
            (thread, previous)
        });
        let _restore = Restore { state: &self.inner,
                                 thread,
                                 previous };
        operation()
    }
}
struct Restore<'a, T: Clone>
{
    state:    &'a ScopedSelection<T>,
    thread:   ThreadId,
    previous: Option<T>
}
impl<T: Clone> Drop for Restore<'_, T>
{
    fn drop(&mut self)
    {
        let displaced = InvocationGate::run(|| {
            let mut state = self.state.state.lock();
            if state.thread == self.thread
            {
                core::mem::replace(&mut state.current, self.previous.take())
            }
            else
            {
                None
            }
        });
        drop(displaced);
    }
}


