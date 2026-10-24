use mac_object_table::{Kind,
                       domain::{CapacityPolicy, CompletionError, DomainKind, KindCode,
                                OwnerIndex, ReleasedResource},
                       projected::{Error, Store},
                       registry::{Registry, Release, Source, Specification},
                       shared::{InvocationGate, OwnerRef}};
struct Gate;
impl InvocationGate for Gate
{
    fn run<R>(operation: impl FnOnce() -> R) -> R
    {
        operation()
    }
}
struct Retain;
impl Release<()> for Retain
{
    fn release(&self, _: &mut (), ticket: ReleasedResource) -> Result<(), CompletionError>
    {
        Err(<CompletionError as mac_object_table::registry::CleanupFailure>::registration_mismatch(ticket))
    }
}
fn owner(kind: DomainKind) -> OwnerRef<Gate>
{
    OwnerRef::try_new(OwnerIndex::issue(kind).unwrap()).unwrap()
}
fn specification(kind: [u8; 4]) -> Specification<'static>
{
    Specification { kind:     KindCode(kind),
                    label:    "Projected fixture",
                    provider: "Fixture",
                    capacity: CapacityPolicy::GrowableUntilMemory,
                    source:   Source::Resident }
}
fn kinds(
    registry: &mut Registry<(), Gate>, registrar: &OwnerRef<Gate>)
    -> (mac_object_table::registry::KindLease<Gate>, mac_object_table::registry::KindLease<Gate>)
{
    (registry.register(registrar, specification(*b"TEST"), Retain).unwrap(),
     registry.register(registrar, specification(*b"TSTB"), Retain).unwrap())
}
#[test]
fn projection_reuse_and_recreated_managers_never_alias_another_owner_or_old_object()
{
    let registrar = owner(DomainKind::Module);
    let mut registry = Registry::new();
    let (payload, backing) = kinds(&mut registry, &registrar);
    let first = owner(DomainKind::Process);
    let second = owner(DomainKind::Process);
    let mut store = Store::new(Kind::Drags, payload.clone(), backing.clone()).unwrap();
    let old = store.insert(&first, "old").unwrap();
    let held = store.remove(old).unwrap();
    assert_eq!(held.wire(), old);
    assert_eq!(*held, "old");
    assert_eq!(store.get(old), None);
    assert_eq!(first.summary().pending, 1);
    drop(held);
    let current = store.insert(&first, "current").unwrap();
    assert_ne!(old, current);
    assert_eq!(old & 0xffff, current & 0xffff);
    assert_eq!(store.get(old), None);
    *store.get_mut(current).unwrap() = "changed";
    assert_eq!(store.iter().collect::<Vec<_>>(), vec![(current, &"changed")]);
    let mut foreign = Store::new(Kind::Drags, payload.clone(), backing.clone()).unwrap();
    let other = foreign.insert(&second, "other").unwrap();
    assert_ne!(current, other);
    assert_eq!(store.get(other), None);
    assert_eq!(foreign.get(current), None);
    drop(store);
    assert_eq!((first.summary().live, first.summary().pending), (0, 0));
    let mut recreated = Store::new(Kind::Drags, payload, backing).unwrap();
    let replacement = recreated.insert(&first, "replacement").unwrap();
    assert_eq!(replacement & 0xffff, old & 0xffff);
    assert_ne!(replacement, old);
    assert_ne!(replacement, current);
    assert_eq!(recreated.get(old), None);
    assert_eq!(recreated.get(current), None);
    assert_eq!(foreign.get(other), Some(&"other"));
}
#[test]
fn invalidation_precedes_cleanup_and_backing_waits_for_the_linear_guard()
{
    let registrar = owner(DomainKind::Module);
    let mut registry = Registry::new();
    let (payload, backing) = kinds(&mut registry, &registrar);
    let ledger = owner(DomainKind::Process);
    let mut store = Store::new(Kind::Drags, payload, backing).unwrap();
    let id = store.insert(&ledger, String::from("retained")).unwrap();
    ledger.stop_admission();
    let ticket = ledger.invalidate_next().unwrap();
    assert_eq!(ticket.key().kind(), KindCode(*b"TEST"));
    assert!(store.get(id).is_none());
    let guard = store.release(ticket).unwrap();
    assert_eq!(&**guard, "retained");
    let storage = ledger.invalidate_next().unwrap();
    store.release_storage(storage).unwrap();
    assert_eq!((ledger.summary().live, ledger.summary().pending), (0, 1));
    assert_eq!(guard.wire(), id);
    drop(guard);
    assert_eq!(ledger.summary().pending, 0);
    let error = store.insert(&ledger, String::new()).unwrap_err();
    assert!(matches!(error, Error::Pool(mac_object_table::domain::PoolError::Stopping { .. })));
    assert!(error.to_string().contains("stopping"));
}
#[test]
fn projection_admission_refusal_never_publishes_an_extra_payload()
{
    let registrar = owner(DomainKind::Module);
    let mut registry = Registry::new();
    let (payload, backing) = kinds(&mut registry, &registrar);
    let ledger = owner(DomainKind::Process);
    let mut store = Store::new(Kind::Drags, payload, backing).unwrap();
    for n in 0..Kind::Drags.capacity()
    {
        store.insert(&ledger, n).unwrap();
    }
    let before = ledger.summary();
    let error = store.insert(&ledger, 100).unwrap_err();
    assert!(matches!(error, Error::ProjectionExhausted));
    assert!(error.to_string().contains("no reusable slot or backing memory"));
    assert_eq!(ledger.summary(), before);
    assert_eq!(store.iter().count(), Kind::Drags.capacity());
}

#[test]
fn pending_cleanup_reserves_the_wire_slot_and_refusal_preserves_every_other_object()
{
    let registrar = owner(DomainKind::Module);
    let mut registry = Registry::new();
    let (payload, backing) = kinds(&mut registry, &registrar);
    let ledger = owner(DomainKind::Process);
    let mut store = Store::new(Kind::FileWatches, payload, backing).unwrap();
    let ids: Vec<_> =
        (0..Kind::FileWatches.capacity()).map(|n| store.insert(&ledger, n).unwrap()).collect();
    assert_eq!(store.get_with_owner(ids[0]), Some((ledger.owner(), &0)));
    let guard = store.remove(ids[0]).unwrap();
    assert_eq!(store.get(ids[0]), None);
    assert_eq!(store.get_with_owner(ids[0]), None);
    let before = ledger.summary();
    let error = store.insert(&ledger, 999).unwrap_err();
    assert!(matches!(error, Error::ProjectionExhausted));
    assert!(error.to_string().contains("no reusable slot or backing memory"));
    assert_eq!(ledger.summary(), before);
    assert_eq!(store.iter().count(), ids.len() - 1);
    assert_eq!(*guard, 0);
    drop(guard);
    let replacement = store.insert(&ledger, 999).unwrap();
    assert_eq!(ids[0] & 0xffff, replacement & 0xffff);
    assert_ne!(ids[0], replacement);
    assert_eq!(store.get(ids[0]), None);
    assert_eq!(store.get(replacement), Some(&999));
    for &id in &ids[1..]
    {
        assert!(store.get(id).is_some());
    }
}

#[test]
fn cleanup_guard_outlives_its_manager_without_losing_the_pending_barrier()
{
    let registrar = owner(DomainKind::Module);
    let mut registry = Registry::new();
    let (payload, backing) = kinds(&mut registry, &registrar);
    let ledger = owner(DomainKind::Process);
    let mut store = Store::new(Kind::FileWatches, payload, backing).unwrap();
    let id = store.insert(&ledger, String::from("provider probe")).unwrap();
    ledger.stop_admission();
    let payload = ledger.invalidate_next().unwrap();
    let held = store.release(payload).unwrap();
    let storage = ledger.invalidate_next().unwrap();
    store.release_storage(storage).unwrap();
    drop(store);
    assert_eq!(&**held, "provider probe");
    assert_eq!(held.wire(), id);
    assert_eq!((ledger.summary().live, ledger.summary().pending), (0, 1));
    drop(held);
    assert_eq!((ledger.summary().live, ledger.summary().pending), (0, 0));
}

#[test]
fn unscoped_wire_namespaces_cannot_construct_a_registered_projection()
{
    let registrar = owner(DomainKind::Module);
    let mut registry = Registry::new();
    let (payload, backing) = kinds(&mut registry, &registrar);
    let error = Store::<u8, Gate>::new(Kind::Files, payload, backing).err().unwrap();
    assert!(matches!(error, Error::UnscopedProjection));
    assert!(error.to_string().contains("requires boot-wide generation history"));
}
