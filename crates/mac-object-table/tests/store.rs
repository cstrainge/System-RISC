use std::sync::{Arc,
                atomic::{AtomicUsize, Ordering}};

use mac_object_table::{domain::{CapacityPolicy, DomainKind, KindCode, OwnerIndex, PoolError},
                       shared::{InvocationGate, OwnerRef},
                       store::Store};
struct Gate;
impl InvocationGate for Gate
{
    fn run<R>(operation: impl FnOnce() -> R) -> R
    {
        operation()
    }
}
const KIND: KindCode = KindCode(*b"CTRL");
fn owner() -> OwnerRef<Gate>
{
    OwnerRef::try_new(OwnerIndex::issue(DomainKind::Process).unwrap()).unwrap()
}
#[test]
fn ownership_and_generation_are_validated_and_native_tokens_cannot_be_classic_ids()
{
    let first = owner();
    let second = owner();
    let mut store = Store::new(KIND, CapacityPolicy::PerDomainQuota { slots: 1 });
    let a = store.insert(&first, "first").unwrap();
    let b = store.insert(&second, "second").unwrap();
    assert_ne!(a, b);
    assert!(a > u64::from(u32::MAX) && b > u64::from(u32::MAX));
    assert_eq!(store.owner(a), Some(first.owner()));
    assert_eq!(store.owner(b), Some(second.owner()));
    assert_eq!(store.get(u64::from(u32::MAX)), None);
    assert_eq!(store.get(first.owner().incarnation()), None);
    let removed = store.remove(a).unwrap();
    assert_eq!(*removed, "first");
    assert!(store.get(a).is_none());
    assert_eq!((first.summary().live, first.summary().pending), (0, 1));
    let error = store.insert(&first, "too early").unwrap_err();
    assert!(matches!(error,
                     PoolError::Exhausted { used: 0,
                                            pending: 1,
                                            limit: 1,
                                            .. }));
    assert!(error.to_string().contains("CTRL") && error.to_string().contains("1 pending cleanup"));
    drop(removed);
    let replacement = store.insert(&first, "replacement").unwrap();
    assert_ne!(a, replacement);
    assert_eq!(first.summary().slots, 1, "replacement must reuse the same physical slot");
    assert!(store.get(a).is_none());
    assert_eq!(store.get(replacement), Some(&"replacement"));
    assert_eq!(store.get(b), Some(&"second"));
    drop(store);
    assert_eq!((first.summary().live, first.summary().pending), (0, 0));
    assert_eq!((second.summary().live, second.summary().pending), (0, 0));
}
#[test]
fn exclusive_storage_cannot_be_bypassed_and_a_guard_can_outlive_its_store()
{
    let ledger = owner();
    let policy = CapacityPolicy::PerDomainQuota { slots: 1 };
    let mut store = Store::new(KIND, policy);
    let id = store.insert(&ledger, 17).unwrap();
    let mut duplicate = Store::new(KIND, policy);
    let error = duplicate.insert(&ledger, 18).unwrap_err();
    assert!(matches!(error, PoolError::ValueStorageAttached { .. }));
    assert!(error.to_string().contains("exclusive value storage"));
    assert!(matches!(ledger.allocate(KIND), Err(PoolError::ValueStorageAttached { .. })));
    assert!(matches!(ledger.grow(KIND, 2), Err(PoolError::ValueStorageAttached { .. })));
    let released = ledger.invalidate_next().unwrap();
    assert!(store.get(id).is_none());
    let refused = ledger.complete_release(released).unwrap_err();
    assert!(matches!(refused.cause(), PoolError::ValueStorageAttached { .. }));
    let removed = store.release(refused.into_ticket()).unwrap();
    drop(store);
    assert_eq!(*removed, 17);
    assert!(matches!(duplicate.insert(&ledger, 18), Err(PoolError::ValueStorageAttached { .. })));
    drop(removed);
    let new = duplicate.insert(&ledger, 18).unwrap();
    assert_ne!(new, id);
    assert_eq!(ledger.summary().slots, 1);
    assert_eq!(duplicate.get(id), None);
}
#[test]
fn wrong_registration_retains_the_walk_ticket_and_payload_cleanup_is_outside_the_lock()
{
    struct Payload
    {
        ledger: OwnerRef<Gate>,
        drops:  Arc<AtomicUsize>
    }
    impl Drop for Payload
    {
        fn drop(&mut self)
        {
            assert_eq!(self.ledger.summary().pending,
                       1,
                       "the lock must be available to destructors");
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    let ledger = owner();
    let drops = Arc::new(AtomicUsize::new(0));
    let mut store = Store::new(KIND, CapacityPolicy::GrowableUntilMemory);
    let id = store.insert(&ledger,
                          Payload { ledger: ledger.clone(),
                                    drops:  drops.clone() })
                  .unwrap();
    let mut wrong: Store<Payload, Gate> =
        Store::new(KindCode(*b"OTHR"), CapacityPolicy::GrowableUntilMemory);
    let ticket = ledger.invalidate_next().unwrap();
    assert!(store.get(id).is_none());
    let error = wrong.release(ticket).err().unwrap();
    assert!(error.to_string().contains("ticket remains owned for retry"));
    drop(store.release(error.into_ticket()).unwrap());
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert_eq!(ledger.summary().pending, 0);
    drop(store);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
}
#[test]
fn growth_has_no_old_global_control_cap_and_one_owners_churn_does_not_spend_anothers_history()
{
    let first = owner();
    let second = owner();
    let mut store = Store::new(KIND, CapacityPolicy::GrowableUntilMemory);
    let ids: Vec<_> = (0..513).map(|i| store.insert(&first, i).unwrap()).collect();
    assert_eq!(first.summary().live, 513);
    assert_eq!(second.summary().live, 0);
    let foreign = store.insert(&second, 900).unwrap();
    store.remove_where(|domain, _| domain == first.owner());
    assert!(ids.iter().all(|&id| store.get(id).is_none()));
    assert_eq!((first.summary().live, first.summary().pending), (0, 0));
    assert_eq!(store.get(foreign), Some(&900));
    assert_eq!(second.summary().slots, 1);
    let fresh = store.insert(&first, 1000).unwrap();
    assert!(!ids.contains(&fresh));
}
#[test]
fn contained_destructor_panic_does_not_falsely_complete_cleanup()
{
    struct Panics;
    impl Drop for Panics
    {
        fn drop(&mut self)
        {
            panic!("cleanup contract failure");
        }
    }
    let ledger = owner();
    let mut store = Store::new(KIND, CapacityPolicy::PerDomainQuota { slots: 1 });
    let id = store.insert(&ledger, Panics).unwrap();
    let removed = store.remove(id).unwrap();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(removed))).is_err());
    assert_eq!((ledger.summary().live, ledger.summary().pending), (0, 1));
    assert!(store.get(id).is_none());
    drop(store);
    assert_eq!(ledger.summary().pending,
               1,
               "failed cleanup is quarantined, not reported reclaimed");
}
#[test]
fn publication_racing_domain_stop_always_has_a_payload_for_the_walk()
{
    for _ in 0..100
    {
        let ledger = owner();
        let peer = ledger.clone();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let worker_barrier = barrier.clone();
        let worker = std::thread::spawn(move || {
            let mut store = Store::new(KIND, CapacityPolicy::GrowableUntilMemory);
            worker_barrier.wait();
            let result = store.insert(&peer, 42);
            (store, result)
        });
        barrier.wait();
        ledger.stop_admission();
        let ticket = ledger.invalidate_next();
        let (mut store, result) = worker.join().unwrap();
        match result
        {
            Ok(id) =>
            {
                let ticket = ticket.expect("publication precedes stop and is visible to the walk");
                assert!(store.get(id).is_none());
                let removed = store.release(ticket).unwrap();
                assert_eq!(*removed, 42);
                drop(removed);
            }
            Err(PoolError::Stopping { .. }) => assert!(ticket.is_none()),
            Err(error) => panic!("unexpected admission refusal: {error}")
        }
        assert_eq!((ledger.summary().live, ledger.summary().pending), (0, 0));
    }
}
