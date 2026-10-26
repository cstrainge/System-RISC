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

#[cfg(test)]
mod owned_subject_tests
{
    extern crate std;
    use alloc::sync::Arc;

    use mac_object_table::{domain::{DomainKind, OwnerIndex, ReleasedResource},
                           registry::Release};

    use super::*;
    use crate::{drivers::jobs::subjects::{Operations, Subject, specification},
                toolbox::handles::{OwnerRef, Registry, cleanup::Failure, subject::Authority}};

    struct Ignore;
    impl Release<(), Failure> for Ignore
    {
        fn release(&self, _: &mut (), ticket: ReleasedResource) -> Result<(), Failure>
        {
            Err(Failure::context_unavailable(ticket, "test must dispatch the actual I/O store"))
        }
    }
    fn owner() -> OwnerRef
    {
        OwnerRef::try_new(OwnerIndex::issue(DomainKind::Process).unwrap()).unwrap()
    }
    fn fixture() -> (Registry<()>, Arc<Operations>)
    {
        let registrar = owner();
        let mut registry = Registry::new();
        let roots = registry.register(&registrar, specification(false), Ignore).unwrap();
        let storage = registry.register(&registrar, specification(true), Ignore).unwrap();
        (registry, Operations::new((roots, storage)).unwrap())
    }
    fn subject(operations: &Arc<Operations>, owner: &OwnerRef) -> Subject
    {
        operations.admit(&Authority::Client(owner.clone())).unwrap()
    }
    #[test]
    fn nested_owned_subjects_restore_the_actual_scope_and_do_not_allocate_a_new_root()
    {
        let (_registry, operations) = fixture();
        let a = owner();
        let b = owner();
        let first = subject(&operations, &a);
        let second = subject(&operations, &b);
        let selections = ScopedSelection::new(ThreadId(1));
        let before = (a.summary().live, b.summary().live);
        selections.with(first.clone(), || {
                      assert!(selections.current().unwrap().scope().same_scope(first.scope()));
                      selections.with(second.clone(), || {
                                    assert!(selections.current()
                                                      .unwrap()
                                                      .scope()
                                                      .same_scope(second.scope()));
                                })
                                .unwrap();
                      assert!(selections.current().unwrap().scope().same_scope(first.scope()));
                  })
                  .unwrap();
        assert!(selections.current().is_none());
        assert_eq!(before, (a.summary().live, b.summary().live));
    }
    #[test]
    fn error_and_host_unwind_release_context_pins_without_claiming_native_unwind_recovery()
    {
        let (_registry, operations) = fixture();
        let owner = owner();
        let first = subject(&operations, &owner);
        let scope = first.scope().clone();
        let root = scope.root().unwrap();
        let selections = ScopedSelection::new(ThreadId(1));
        let failed: Result<(), mac_abi::Status> =
            selections.with(first.clone(), || Err(mac_abi::NOT_FOUND)).unwrap();
        assert_eq!(failed, Err(mac_abi::NOT_FOUND));
        assert!(selections.current().is_none());
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                                  selections.with(first, || {
                                                                assert!(selections.current()
                                                                                  .is_some());
                                                                panic!("host subject fixture");
                                                            })
                                                            .unwrap();
                                              }));
        assert!(failed.is_err());
        assert!(selections.current().is_none());
        assert!(!owner.contains(root), "context retained the completed operation root");
        assert!(!owner.pending(root));
        assert!(scope.admitting().is_err());
    }
    #[test]
    fn stopped_operation_can_finish_in_its_frame_without_reopening_client_admission()
    {
        let (_registry, operations) = fixture();
        let owner = owner();
        let first = subject(&operations, &owner);
        let root = first.scope().root().unwrap();
        let selections = ScopedSelection::new(ThreadId(1));
        owner.stop_admission();
        let ticket = owner.invalidate(root).unwrap();
        operations.release(ticket, false).unwrap();
        selections.with(first, || {
                      let retained = selections.current().unwrap();
                      retained.scope().admitting().unwrap();
                      assert!(matches!(operations.admit(&Authority::Client(owner.clone())),
                                       Err(mac_abi::SERVICE_BUSY)));
                      assert!(owner.pending(root));
                  })
                  .unwrap();
        assert!(!owner.pending(root));
    }
    #[test]
    fn old_owned_restore_drops_its_pins_without_changing_a_reused_thread()
    {
        let (_registry, operations) = fixture();
        let owner = owner();
        let first = subject(&operations, &owner);
        let root = first.scope().root().unwrap();
        let second = subject(&operations, &owner);
        let next = second.scope().clone();
        let mut threads = mac_object_table::Table::new(mac_object_table::Kind::Threads);
        let old = threads.insert(()).unwrap();
        let selections = ScopedSelection::new(ThreadId(u64::from(old)));
        selections.with(first, || {
                      threads.remove(old).unwrap();
                      let fresh = threads.insert(()).unwrap();
                      assert_ne!(old, fresh);
                      selections.reset(ThreadId(u64::from(fresh)));
                      selections.with(second, || {
                                    assert!(selections.current()
                                                      .unwrap()
                                                      .scope()
                                                      .same_scope(&next));
                                })
                                .unwrap();
                  })
                  .unwrap();
        assert!(selections.current().is_none());
        assert!(!owner.contains(root));
        assert!(!owner.pending(root));
    }
    #[test]
    fn simultaneous_thread_contexts_never_borrow_another_operations_subject()
    {
        use std::sync::Barrier;
        let (_registry, operations) = fixture();
        let barrier = Arc::new(Barrier::new(2));
        let mut workers = alloc::vec::Vec::new();
        for _ in 0..2
        {
            let owner = owner();
            let subject = subject(&operations, &owner);
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                             let scope = subject.scope().clone();
                             let selections =
                                 ScopedSelection::new(ThreadId(owner.owner().incarnation()));
                             selections.with(subject, || {
                                           barrier.wait();
                                           assert!(selections.current()
                                                             .unwrap()
                                                             .scope()
                                                             .same_scope(&scope));
                                           barrier.wait();
                                       })
                                       .unwrap();
                             assert!(selections.current().is_none());
                             assert!(scope.admitting().is_err());
                         }));
        }
        for worker in workers
        {
            worker.join().unwrap();
        }
    }
}

#[cfg(test)]
mod destruction_tests
{
    use alloc::sync::{Arc, Weak};
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    // A destructor may reenter the context. This catches holding its lock while
    // destroying a replaced owned capability, without relying on a timeout.
    #[derive(Clone)]
    struct Probe
    {
        slot: Weak<ScopedSelection<Probe>>,
        seen: Arc<AtomicUsize>
    }
    impl Drop for Probe
    {
        fn drop(&mut self)
        {
            if let Some(slot) = self.slot.upgrade()
            {
                assert!(slot.state.try_lock().is_some(), "destructor ran under selection lock");
                self.seen.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    #[test]
    fn restoring_and_resetting_owned_frames_destroy_after_unlock()
    {
        let slot = Arc::new(ScopedSelection::new(ThreadId(1)));
        let seen = Arc::new(AtomicUsize::new(0));
        let probe = || {
            Probe { slot: Arc::downgrade(&slot),
                    seen: seen.clone() }
        };
        slot.with(probe(), || {
                slot.with(probe(), || ()).unwrap();
                assert_eq!(seen.load(Ordering::Relaxed), 1);
                slot.reset(ThreadId(0));
                assert_eq!(seen.load(Ordering::Relaxed), 2);
                assert_eq!(slot.with(probe(), || panic!("reclaimed context executed a callback")),
                           Err(Reclaimed));
            })
            .unwrap();
        assert_eq!(seen.load(Ordering::Relaxed), 3);
        assert!(slot.current().is_none());
    }
}
