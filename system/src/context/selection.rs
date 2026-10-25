//! Thread-owned selection metadata, never an image/payload lifetime pin.
//! Guards own no allocation on an abandoned stack. The scheduler's shared-
//! facility thread identity prevents an old restore from changing a reused
//! context. Actual image aliases/pins belong to the later image migration.
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
pub(super) struct ScopedSelection<T: Copy>
{
    state: Spinlock<State<T>>
}
impl<T: Copy> ScopedSelection<T>
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
        InvocationGate::run(|| {
            let mut state = self.state.lock();
            state.thread = thread;
            state.current = None;
        });
    }

    pub(super) fn current(&self) -> Option<T>
    {
        InvocationGate::run(|| self.state.lock().current)
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
pub(super) struct PermanentSelection<T: Copy>
{
    inner: ScopedSelection<T>
}
impl<T: Copy> PermanentSelection<T>
{
    pub(super) const fn new(thread: ThreadId) -> Self
    {
        Self { inner: ScopedSelection::new(thread) }
    }

    pub(super) fn current(&self) -> Option<T>
    {
        self.inner.current()
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
struct Restore<'a, T: Copy>
{
    state:    &'a ScopedSelection<T>,
    thread:   ThreadId,
    previous: Option<T>
}
impl<T: Copy> Drop for Restore<'_, T>
{
    fn drop(&mut self)
    {
        InvocationGate::run(|| {
            let mut state = self.state.state.lock();
            if state.thread == self.thread
            {
                state.current = self.previous;
            }
        });
    }
}
#[cfg(test)]
mod tests
{
    extern crate std;
    use alloc::string::ToString;

    use mac_object_table::{Kind, Table};

    use super::*;
    fn threads() -> (Table<()>, ThreadId)
    {
        let mut table = Table::new(Kind::Threads);
        let id = ThreadId(u64::from(table.insert(()).unwrap()));
        (table, id)
    }
    #[test]
    fn nested_resource_selections_restore_only_their_own_thread()
    {
        let (mut identities, a) = threads();
        let b = ThreadId(u64::from(identities.insert(()).unwrap()));
        assert_ne!(a, b);
        let first = ScopedSelection::new(a);
        let other = ScopedSelection::new(b);
        first.with((11, 3), || {
                 assert_eq!(first.current(), Some((11, 3)));
                 assert_eq!(other.current(), None);
                 other.with((22, 4), || {
                          first.with((11, 9), || {
                                   assert_eq!(first.current(), Some((11, 9)));
                                   assert_eq!(other.current(), Some((22, 4)));
                               })
                               .unwrap();
                          assert_eq!(first.current(), Some((11, 3)));
                      })
                      .unwrap();
                 assert_eq!(other.current(), None);
             })
             .unwrap();
        assert_eq!(first.current(), None);
    }
    #[test]
    fn retired_and_reused_context_rejects_the_old_threads_restore()
    {
        let (mut table, old) = threads();
        let selections = ScopedSelection::new(old);
        selections.with((11, 3), || {
                      selections.with((11, 9), || {
                                    table.remove(u32::try_from(old.0).unwrap()).unwrap();
                                    let fresh = ThreadId(u64::from(table.insert(()).unwrap()));
                                    assert_eq!(old.0 & 0xffff, fresh.0 & 0xffff);
                                    assert_ne!(old, fresh);
                                    selections.reset(ThreadId(0));
                                    let error = selections.with((11, 7), || {
                                                              panic!("reclaimed context ran a \
                                                                      resource callback")
                                                          })
                                                          .unwrap_err();
                                    assert!(error.to_string().contains("context was reclaimed"));
                                    selections.reset(fresh);
                                    selections.with((22, 4), || {
                                                  assert_eq!(selections.current(), Some((22, 4)))
                                              })
                                              .unwrap();
                                })
                                .unwrap();
                      assert_eq!(selections.current(),
                                 None,
                                 "old restore leaked the prior app into the new thread");
                      selections.with((22, 5), || assert_eq!(selections.current(), Some((22, 5))))
                                .unwrap();
                  })
                  .unwrap();
        assert_eq!(selections.current(), None);
    }
    #[test]
    fn unwinding_a_host_callback_restores_its_selection_without_a_native_recovery_claim()
    {
        let (_table, thread) = threads();
        let selections = ScopedSelection::new(thread);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                                  selections.with((11, 3), || {
                                                                selections.with((11, 9), || {
                                                                    panic!("host selection fixture")
                                                                })
                                                                .unwrap();
                                                            })
                                                            .unwrap();
                                              }));
        assert!(result.is_err());
        assert_eq!(selections.current(), None);
        selections.with((11, 4), || assert_eq!(selections.current(), Some((11, 4)))).unwrap();
    }
    #[test]
    fn simultaneous_host_threads_do_not_replace_each_others_selected_resources()
    {
        use std::sync::{Arc, Barrier};
        let gate = Arc::new(Barrier::new(2));
        let mut workers = std::vec::Vec::new();
        let mut identities = Table::new(Kind::Threads);
        for owner in [11, 22]
        {
            let gate = gate.clone();
            let thread = ThreadId(u64::from(identities.insert(()).unwrap()));
            workers.push(std::thread::spawn(move || {
                             let selections = ScopedSelection::new(thread);
                             selections.with((owner, 3), || {
                                           gate.wait();
                                           assert_eq!(selections.current(), Some((owner, 3)));
                                           gate.wait();
                                       })
                                       .unwrap();
                             assert_eq!(selections.current(), None);
                         }));
        }
        for worker in workers
        {
            worker.join().unwrap();
        }
    }
}

#[cfg(test)]
mod handoff_tests
{
    use super::*;
    #[test]
    fn bootstrap_handoff_carries_the_callers_selection_and_restores_both_threads()
    {
        let mut threads = mac_object_table::Table::new(mac_object_table::Kind::Threads);
        let thread = ThreadId(u64::from(threads.insert(()).unwrap()));
        let caller = ScopedSelection::new(thread);
        let bootstrap = PermanentSelection::new(ThreadId(1));
        bootstrap.with(Some((0, 0)), || {
                     caller.with((11, 3), || {
                               bootstrap.with(caller.current(), || {
                                            assert_eq!(bootstrap.current(), Some((11, 3)));
                                            bootstrap.with(Some((22, 9)), || {
                                                         assert_eq!(bootstrap.current(),
                                                                    Some((22, 9)))
                                                     });
                                            assert_eq!(bootstrap.current(), Some((11, 3)));
                                        });
                               assert_eq!(caller.current(), Some((11, 3)));
                               assert_eq!(bootstrap.current(), Some((0, 0)));
                           })
                           .unwrap();
                     bootstrap.with(caller.current(), || assert_eq!(bootstrap.current(), None));
                     assert_eq!(bootstrap.current(), Some((0, 0)));
                 });
        assert_eq!(caller.current(), None);
        assert_eq!(bootstrap.current(), None);
    }
}
