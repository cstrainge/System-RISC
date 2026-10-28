//! Invalidation revokes identity immediately, but payload cleanup owns reuse.
use mac_object_table::{domain::{CapacityPolicy, DirectoryError, DomainKind, KindCode, OwnerIndex,
                                PoolError},
                       shared::{InvocationGate, OwnerRef}};
struct Gate;
impl InvocationGate for Gate
{
    fn run<R>(operation: impl FnOnce() -> R) -> R
    {
        operation()
    }
}
type Directory = mac_object_table::domain::Directory<(), Gate>;
const KIND: KindCode = KindCode(*b"CTRL");

fn owner(directory: &mut Directory) -> mac_object_table::domain::DomainId
{
    let mut index = OwnerIndex::issue(DomainKind::Process).unwrap();
    index.add_pool(KIND, CapacityPolicy::PerDomainQuota { slots: 1 }).unwrap();
    index.grow(KIND, 1).unwrap();
    directory.insert(OwnerRef::try_new(index).unwrap(), ()).unwrap()
}

#[test]
fn cleanup_holds_reuse_and_retirement_and_wrong_owner_refusal_retains_the_ticket()
{
    let mut directory = Directory::new();
    let first = owner(&mut directory);
    let second = owner(&mut directory);
    let ledger = directory.get(first.incarnation()).unwrap().ledger().clone();
    let other = directory.get(second.incarnation()).unwrap().ledger().clone();
    let old = ledger.allocate(KIND).unwrap();
    let release = ledger.invalidate(old).unwrap();
    assert!(!ledger.contains(old));
    assert_eq!((ledger.summary().live, ledger.summary().pending), (0, 1));
    let error = ledger.allocate(KIND).unwrap_err();
    assert!(matches!(error,
                     PoolError::Exhausted { used: 0,
                                            pending: 1,
                                            limit: 1,
                                            .. }));
    assert!(error.to_string().contains("1 pending cleanup"));
    assert!(error.to_string().contains(&first.to_string()));
    assert!(error.to_string().contains("CTRL"));
    assert_eq!(directory.remove(first.incarnation()),
               Err(DirectoryError::PendingResources { owner:   first,
                                                      pending: 1 }));
    let refused = other.complete_release(release).unwrap_err();
    assert!(matches!(refused.cause(),PoolError::ForeignOwner { expected,actual } if expected==second && actual==first));
    assert!(refused.to_string().contains("ticket remains owned for retry"));
    ledger.complete_release(refused.into_ticket()).unwrap();
    assert_eq!(ledger.summary().pending, 0);
    let new = ledger.allocate(KIND).unwrap();
    assert_eq!(old.slot(), new.slot());
    assert_ne!(old.generation(), new.generation());
    assert!(!ledger.contains(old));
    let release = ledger.invalidate(new).unwrap();
    ledger.complete_release(release).unwrap();
    directory.remove(first.incarnation()).unwrap();
}

#[test]
fn allocation_racing_cleanup_cannot_publish_before_the_payload_is_released()
{
    use std::sync::{Arc, Barrier,
                    atomic::{AtomicBool, Ordering}};
    for _ in 0..100
    {
        let mut directory = Directory::new();
        let owner = owner(&mut directory);
        let ledger = directory.get(owner.incarnation()).unwrap().ledger().clone();
        let old = ledger.allocate(KIND).unwrap();
        let release = ledger.invalidate(old).unwrap();
        let freed = Arc::new(AtomicBool::new(false));
        let start = Arc::new(Barrier::new(2));
        let peer = ledger.clone();
        let peer_freed = freed.clone();
        let peer_start = start.clone();
        let worker = std::thread::spawn(move || {
            peer_start.wait();
            let allocated = peer.allocate(KIND);
            let observed_free = peer_freed.load(Ordering::Acquire);
            (allocated, observed_free)
        });
        start.wait();
        freed.store(true, Ordering::Release); // Represents completed payload destruction.
        ledger.complete_release(release).unwrap();
        match worker.join().unwrap()
        {
            (Ok(key), true) =>
            {
                assert_ne!(key, old);
                let release = ledger.invalidate(key).unwrap();
                ledger.complete_release(release).unwrap();
            }
            (Err(PoolError::Exhausted { pending: 1,
                                        .. }),
             _) => (),
            unexpected => panic!("published before cleanup or lost admission state: {unexpected:?}")
        }
        directory.remove(owner.incarnation()).unwrap();
    }
}
