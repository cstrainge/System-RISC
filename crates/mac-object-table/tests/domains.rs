use mac_object_table::domain::{CapacityPolicy, Directory, DirectoryError, DomainKind, KindCode,
                               OwnerIndex};

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
