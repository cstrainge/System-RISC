use mac_object_table::{domain::{CapacityPolicy, DirectoryError, DomainKind, KindCode, OwnerIndex},
                       shared::InvocationGate};
struct HostGate;
impl InvocationGate for HostGate
{
    fn run<R>(operation: impl FnOnce() -> R) -> R
    {
        operation()
    }
}
type Directory<T> = mac_object_table::domain::Directory<T, HostGate>;

#[test]
fn directory_preserves_owner_identity_and_has_no_fixed_eight_limit()
{
    let mut directory = Directory::new();
    assert!(directory.is_empty());
    let indices: Vec<_> =
        (0..32).map(|_| OwnerIndex::issue(DomainKind::Process).unwrap()).collect();
    let owners: Vec<_> = indices.iter().map(OwnerIndex::owner).collect();
    // Admission may complete out of issue order. Lookup is ordered by immutable
    // incarnation, not insertion order or a mutable payload's ID field.
    for (index, ledger) in indices.into_iter().enumerate().rev()
    {
        assert_eq!(directory.insert(ledger, index).unwrap(), owners[index]);
    }
    assert_eq!(directory.len(), 32);
    for (index, owner) in owners.iter().enumerate()
    {
        let entry = directory.get(owner.incarnation()).unwrap();
        assert_eq!(entry.owner(), *owner);
        assert_eq!(entry.ledger().owner(), *owner);
        assert_eq!(*entry.value(), index);
    }
    let ordered: Vec<_> = directory.iter().map(|entry| entry.owner()).collect();
    assert_eq!(ordered, owners);
    *directory.get_mut(owners[0].incarnation()).unwrap() = 99;
    assert_eq!(directory.get(owners[0].incarnation()).unwrap().owner(), owners[0]);
    for value in directory.values_mut()
    {
        *value += 1;
    }
    assert_eq!(directory.get(owners[0].incarnation()).unwrap().value(), &100);
}

#[test]
fn retired_domain_cannot_alias_reused_directory_storage_or_a_new_directory()
{
    let mut directory = Directory::new();
    let first = directory.insert(OwnerIndex::issue(DomainKind::Process).unwrap(), "first").unwrap();
    assert_eq!(directory.remove(first.incarnation()).unwrap(), "first");
    let next = directory.insert(OwnerIndex::issue(DomainKind::Process).unwrap(), "next").unwrap();
    assert!(next.incarnation() > first.incarnation());
    assert!(directory.get(first.incarnation()).is_none());
    assert!(directory.get_mut(first.incarnation()).is_none());
    assert_eq!(directory.remove(first.incarnation()),
               Err(DirectoryError::NotFound { incarnation: first.incarnation() }));
    drop(directory);
    let mut recreated = Directory::new();
    recreated.insert(OwnerIndex::issue(DomainKind::Process).unwrap(), "recreated").unwrap();
    assert!(recreated.get(first.incarnation()).is_none());
    assert!(recreated.get(next.incarnation()).is_none());
    assert!(recreated.remove(next.incarnation()).unwrap_err().to_string().contains("not found"));
}

#[test]
fn retirement_refuses_live_resources_and_succeeds_only_after_invalidation()
{
    let mut ledger = OwnerIndex::issue(DomainKind::Process).unwrap();
    let kind = KindCode(*b"TEST");
    ledger.add_pool(kind, CapacityPolicy::GrowableUntilMemory).unwrap();
    ledger.grow(kind, 1).unwrap();
    let key = ledger.allocate(kind).unwrap();
    let mut directory = Directory::new();
    let owner = directory.insert(ledger, "payload remains available").unwrap();
    let error = directory.remove(owner.incarnation()).unwrap_err();
    assert_eq!(error,
               DirectoryError::LiveResources { owner,
                                               live: 1 });
    let detail = error.to_string();
    assert!(detail.contains(&owner.to_string()));
    assert!(detail.contains("1 live ledger resources"));
    assert!(directory.get(owner.incarnation()).unwrap().ledger().contains(key));
    directory.stop_admission(owner.incarnation()).unwrap();
    let ticket = directory.invalidate_next(owner.incarnation()).unwrap().unwrap();
    assert_eq!(ticket.key(), key);
    assert!(!directory.get(owner.incarnation()).unwrap().ledger().contains(key));
    assert!(directory.invalidate_next(owner.incarnation()).unwrap().is_none());
    assert_eq!(directory.remove(owner.incarnation()).unwrap(), "payload remains available");
    assert!(directory.is_empty());
    assert_eq!(directory.stop_admission(owner.incarnation()),
               Err(DirectoryError::NotFound { incarnation: owner.incarnation() }));
    assert!(matches!(directory.invalidate_next(owner.incarnation()),
                     Err(DirectoryError::NotFound { .. })));
}

#[test]
fn manager_references_share_one_ledger_and_cannot_reopen_a_retired_owner()
{
    let mut directory = Directory::new();
    let owner = directory.insert(OwnerIndex::issue(DomainKind::Process).unwrap(), ()).unwrap();
    let first = directory.get(owner.incarnation()).unwrap().ledger().clone();
    let second = first.clone();
    let kind = KindCode(*b"SHAR");
    first.add_pool(kind, CapacityPolicy::GrowableUntilMemory).unwrap();
    first.grow(kind, 1).unwrap();
    let key = first.allocate(kind).unwrap();
    assert!(second.contains(key));
    assert_eq!(second.live(), 1);
    second.invalidate(key).unwrap();
    assert!(!first.contains(key));
    directory.remove(owner.incarnation()).unwrap();
    for ledger in [first, second]
    {
        assert_eq!(ledger.owner(), owner);
        assert!(matches!(ledger.allocate(kind), Err(mac_object_table::domain::PoolError::Stopping { owner: actual }) if actual == owner));
        assert!(ledger.grow(kind, 2).unwrap_err().to_string().contains("stopping"));
        assert!(!ledger.contains(key));
    }
}

#[test]
fn concurrent_admission_cannot_slip_between_empty_check_and_retirement()
{
    use std::sync::{Arc, Barrier};

    use mac_object_table::domain::PoolError;
    let kind = KindCode(*b"RACE");
    for _ in 0..100
    {
        let mut directory = Directory::new();
        let owner = directory.insert(OwnerIndex::issue(DomainKind::Process).unwrap(), ()).unwrap();
        let ledger = directory.get(owner.incarnation()).unwrap().ledger().clone();
        ledger.add_pool(kind, CapacityPolicy::GrowableUntilMemory).unwrap();
        ledger.grow(kind, 1).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let peer = barrier.clone();
        let reference = ledger.clone();
        let worker = std::thread::spawn(move || {
            peer.wait();
            reference.allocate(kind)
        });
        barrier.wait();
        let retired = directory.remove(owner.incarnation());
        let allocated = worker.join().unwrap();
        match (retired, allocated)
        {
            (Ok(()), Err(PoolError::Stopping { owner: actual })) => assert_eq!(actual, owner),
            (Err(DirectoryError::LiveResources { owner: actual,
                                                 live: 1 }),
             Ok(key)) =>
            {
                assert_eq!(actual, owner);
                assert!(ledger.contains(key));
                ledger.invalidate(key).unwrap();
                directory.remove(owner.incarnation()).unwrap();
            }
            other => panic!("admission and retirement disagreed: {other:?}")
        }
        assert!(directory.is_empty());
        assert_eq!(ledger.live(), 0);
    }
}

#[test]
fn dropping_directory_closes_admission_without_losing_raii_cleanup_keys()
{
    let mut directory = Directory::new();
    let owner = directory.insert(OwnerIndex::issue(DomainKind::Process).unwrap(), ()).unwrap();
    let reference = directory.get(owner.incarnation()).unwrap().ledger().clone();
    let kind = KindCode(*b"DROP");
    reference.add_pool(kind, CapacityPolicy::GrowableUntilMemory).unwrap();
    reference.grow(kind, 1).unwrap();
    let key = reference.allocate(kind).unwrap();
    drop(directory);
    assert!(!reference.accepting());
    assert!(reference.contains(key));
    assert!(matches!(reference.allocate(kind),
                     Err(mac_object_table::domain::PoolError::Stopping { .. })));
    reference.invalidate(key).unwrap();
    assert_eq!(reference.live(), 0);
    assert!(!reference.contains(key));
}
