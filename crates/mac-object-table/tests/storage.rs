use mac_object_table::{domain::{CapacityPolicy, CompletionError, DomainKind, KindCode,
                                OwnerIndex, ReleasedResource},
                       registry::{Registry, Release, Source, Specification},
                       shared::{InvocationGate, OwnerRef},
                       store::Store,
                       walk};

struct Gate;
impl InvocationGate for Gate
{
    fn run<R>(operation: impl FnOnce() -> R) -> R
    {
        operation()
    }
}
const OBJECT: KindCode = KindCode(*b"TEST");
const STATE: KindCode = KindCode(*b"STAT");
struct Context
{
    objects: Store<u8, Gate>
}
struct Cleanup;
impl Release<Context> for Cleanup
{
    fn release(&self, context: &mut Context, ticket: ReleasedResource)
               -> Result<(), CompletionError>
    {
        if ticket.key().kind() == STATE
        {
            context.objects.release_storage(ticket)
        }
        else
        {
            context.objects.release(ticket).map(drop)
        }
    }
}
fn setup() -> (Registry<Context, Gate>, OwnerRef<Gate>, Context)
{
    let registrar = OwnerRef::try_new(OwnerIndex::issue(DomainKind::Module).unwrap()).unwrap();
    let owner = OwnerRef::try_new(OwnerIndex::issue(DomainKind::Process).unwrap()).unwrap();
    let mut registry = Registry::new();
    let mut register = |kind, label, capacity| {
        registry.register(&registrar,
                          Specification { kind,
                                          label,
                                          provider: "Storage fixture",
                                          capacity,
                                          source: Source::Resident },
                          Cleanup)
                .unwrap()
    };
    let object = register(OBJECT, "Objects", CapacityPolicy::GrowableUntilMemory);
    let state = register(STATE,
                         "Backing",
                         CapacityPolicy::HardBound { slots:  1,
                                                     reason: "One backing per domain" });
    let context = Context { objects: Store::owned(object, state).unwrap() };
    (registry, owner, context)
}

#[test]
fn backing_first_waits_for_payloads_without_a_finish_owner_hook()
{
    let (registry, owner, mut context) = setup();
    let id = context.objects.insert(&owner, 42).unwrap();
    let object = owner.invalidate_next().unwrap();
    let state = owner.invalidate_next().unwrap();
    assert_eq!(state.key().kind(), STATE);
    let key = state.key();
    registry.dispatch(STATE).unwrap().invoke(&mut context, state).unwrap();
    assert!(owner.pending(key));
    assert_eq!(owner.summary().pending, 2);
    registry.dispatch(OBJECT).unwrap().invoke(&mut context, object).unwrap();
    assert_eq!((owner.summary().live, owner.summary().pending), (0, 0));
    assert!(context.objects.get(id).is_none());
    // Successful reattachment proves backing and its exclusive storage claim
    // were freed, while the ledger's old generation history remains intact.
    let replacement = context.objects.insert(&owner, 43).unwrap();
    assert_ne!(id, replacement);
    assert_eq!(context.objects.get(replacement), Some(&43));
    assert_eq!(owner.summary().slots, 2);
}

#[test]
fn cached_empty_backing_is_visible_and_drained_even_without_objects()
{
    let (registry, owner, mut context) = setup();
    let id = context.objects.insert(&owner, 42).unwrap();
    drop(context.objects.remove(id));
    assert_eq!(owner.summary().live, 1, "the retained cache is ledger-owned");
    let report = walk::drain(&mut context,
                             &owner,
                             |_, key| registry.dispatch(key.kind()),
                             |_, error| panic!("{error}")).unwrap();
    assert_eq!((report.invalidated, report.released, report.deferred, report.quarantined),
               (1, 1, 0, 0));
    assert_eq!((owner.summary().live, owner.summary().pending), (0, 0));
    assert_eq!(registry.by_id(registry.snapshot().unwrap().0, registry.get(STATE).unwrap().id())
                       .unwrap()
                       .uses,
               0);
}

#[test]
fn quarantined_child_retains_backing_until_the_actual_retry()
{
    let (registry, owner, mut context) = setup();
    context.objects.insert(&owner, 42).unwrap();
    let child = owner.invalidate_next().unwrap();
    let state = owner.invalidate_next().unwrap();
    registry.dispatch(STATE).unwrap().invoke(&mut context, state).unwrap();
    owner.quarantine(child).unwrap();
    assert_eq!((owner.summary().pending, owner.summary().quarantined), (2, 1));
    let result = walk::retry(&mut context,
                             &owner,
                             |_, key| registry.dispatch(key.kind()),
                             |_, error| panic!("{error}")).unwrap();
    assert_eq!((result.released, result.quarantined), (1, 0));
    assert_eq!((owner.summary().live, owner.summary().pending, owner.summary().quarantined),
               (0, 0, 0));
}

#[test]
fn foreign_backing_refusal_retains_ticket_and_original_storage()
{
    let (registry, owner, mut context) = setup();
    let (_other_registry, foreign, mut other) = setup();
    let id = context.objects.insert(&owner, 42).unwrap();
    let other_id = other.objects.insert(&foreign, 13).unwrap();
    let child = owner.invalidate_next().unwrap();
    let state = owner.invalidate_next().unwrap();
    let error = other.objects.release_storage(state).unwrap_err();
    assert!(error.to_string().contains("ticket remains owned for retry"));
    assert_eq!(other.objects.get(other_id), Some(&13));
    registry.dispatch(STATE).unwrap().invoke(&mut context, error.into_ticket()).unwrap();
    registry.dispatch(OBJECT).unwrap().invoke(&mut context, child).unwrap();
    assert_eq!(owner.summary().pending, 0);
    assert!(context.objects.get(id).is_none());
}

#[test]
fn walk_reports_backing_deferred_without_mistaking_it_for_completed_cleanup()
{
    let (registry, owner, mut context) = setup();
    owner.add_pool(STATE,
                   CapacityPolicy::HardBound { slots:  1,
                                               reason: "One backing per domain" })
         .unwrap();
    context.objects.insert(&owner, 42).unwrap();
    let report = walk::drain(&mut context,
                             &owner,
                             |_, key| registry.dispatch(key.kind()),
                             |_, error| panic!("{error}")).unwrap();
    assert_eq!((report.invalidated, report.released, report.deferred, report.quarantined),
               (2, 1, 1, 0));
    assert_eq!((owner.summary().live, owner.summary().pending), (0, 0));
}

#[test]
fn abandoned_store_does_not_destroy_quarantined_payload_or_unpin_its_provider()
{
    use std::sync::{Arc,
                    atomic::{AtomicUsize, Ordering}};
    struct Payload(Arc<AtomicUsize>);
    impl Drop for Payload
    {
        fn drop(&mut self)
        {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let (registry, owner, _context) = setup();
    let dropped = Arc::new(AtomicUsize::new(0));
    let mut store =
        Store::owned(registry.get(OBJECT).unwrap(), registry.get(STATE).unwrap()).unwrap();
    store.insert(&owner, Payload(dropped.clone())).unwrap();
    let child = owner.invalidate_next().unwrap();
    let state = owner.invalidate_next().unwrap();
    store.release_storage(state).unwrap();
    owner.quarantine(child).unwrap();
    drop(store);
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    assert_eq!((owner.summary().pending, owner.summary().quarantined), (2, 1));
    let revision = registry.snapshot().unwrap().0;
    assert_eq!(registry.by_id(revision, registry.get(OBJECT).unwrap().id()).unwrap().uses, 1);
    assert_eq!(registry.by_id(revision, registry.get(STATE).unwrap().id()).unwrap().uses, 1);
}

#[test]
fn refused_first_object_still_leaves_backing_owned_and_cleanable()
{
    let (mut registry, owner, _context) = setup();
    let registrar = OwnerRef::try_new(OwnerIndex::issue(DomainKind::Module).unwrap()).unwrap();
    let disabled = registry.register(&registrar,
                             Specification { kind:     KindCode(*b"NONE"),
                                             label:    "Disabled objects",
                                             provider: "Storage fixture",
                                             capacity: CapacityPolicy::PerDomainQuota { slots: 0 },
                                             source:   Source::Resident },
                             Cleanup)
                   .unwrap();
    let mut store = Store::owned(disabled, registry.get(STATE).unwrap()).unwrap();
    let error = store.insert(&owner, 42).unwrap_err();
    assert!(error.to_string().contains("NONE") && error.to_string().contains("0"));
    assert_eq!(owner.summary().live, 1, "failed publication cannot orphan allocated backing");
    let ticket = owner.invalidate_next().unwrap();
    assert_eq!(ticket.key().kind(), STATE);
    store.release_storage(ticket).unwrap();
    assert_eq!((owner.summary().live, owner.summary().pending), (0, 0));
}

#[test]
fn saved_state_is_scoped_cached_and_dies_with_its_registered_backing()
{
    use std::sync::{Arc,
                    atomic::{AtomicUsize, Ordering}};
    struct Saved
    {
        marker:   u8,
        observer: Option<(OwnerRef<Gate>, Arc<AtomicUsize>)>
    }
    impl Drop for Saved
    {
        fn drop(&mut self)
        {
            if let Some((owner, dropped)) = &self.observer
            {
                assert!(owner.summary().pending > 0,
                        "saved-state destruction must precede backing acknowledgment and run \
                         outside the lock");
                dropped.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
    let (registry, owner, _context) = setup();
    let mut store = Store::<u8, Gate, Saved>::owned_state(registry.get(OBJECT).unwrap(),
                                                          registry.get(STATE).unwrap(),
                                                          |_| {
                                                              Ok(Saved { marker:   7,
                                                                         observer: None })
                                                          }).unwrap();
    assert!(store.state(owner.owner().incarnation()).is_none());
    let id = store.insert(&owner, 42).unwrap();
    let dropped = Arc::new(AtomicUsize::new(0));
    let incarnation = owner.owner().incarnation();
    let saved = store.state_mut(incarnation).unwrap();
    saved.marker = 19;
    saved.observer = Some((owner.clone(), dropped.clone()));
    assert!(store.state(incarnation + 1).is_none());
    drop(store.remove(id));
    assert_eq!(store.state(incarnation).unwrap().marker, 19);
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    let ticket = owner.invalidate_next().unwrap();
    assert!(store.state(incarnation).is_none());
    assert!(store.state_mut(incarnation).is_none());
    store.release_storage(ticket).unwrap();
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert_eq!(owner.summary().pending, 0);
    store.insert(&owner, 43).unwrap();
    assert_eq!(store.state(incarnation).unwrap().marker, 7, "reattachment constructs fresh state");
    drop(registry);
    assert!(store.state(incarnation).is_none(),
            "abandoned registration cannot authorize state access");
}

#[test]
fn saved_state_factory_refusal_does_not_publish_or_poison_a_domain()
{
    use mac_object_table::domain::PoolError;
    let (registry, owner, _context) = setup();
    let mut store = Store::<u8, Gate, ()>::owned_state(registry.get(OBJECT).unwrap(),
                                                       registry.get(STATE).unwrap(),
                                                       |owner| {
                                                           Err(PoolError::Storage { owner,
                                                                        kind: STATE,
                                                                        used: 0,
                                                                        requested_slots: 1,
                                                                        limit: None })
                                                       }).unwrap();
    let error = store.insert(&owner, 42).unwrap_err();
    assert!(matches!(error,
                     PoolError::Storage { kind: STATE,
                                          .. }));
    assert!(error.to_string().contains("STAT"));
    assert_eq!((owner.summary().live, owner.summary().pending), (0, 0));
    drop(store);
    let mut retry =
        Store::owned(registry.get(OBJECT).unwrap(), registry.get(STATE).unwrap()).unwrap();
    retry.insert(&owner, 43).unwrap();
    assert_eq!(owner.summary().live, 2);
}

#[test]
fn saved_state_panic_keeps_backing_pending_and_provider_pinned()
{
    struct Saved;
    impl Drop for Saved
    {
        fn drop(&mut self)
        {
            panic!("saved-state destruction refused");
        }
    }
    let (registry, owner, _context) = setup();
    let mut store = Store::<u8, Gate, Saved>::owned_state(registry.get(OBJECT).unwrap(),
                                                          registry.get(STATE).unwrap(),
                                                          |_| Ok(Saved)).unwrap();
    let id = store.insert(&owner, 42).unwrap();
    drop(store.remove(id));
    let ticket = owner.invalidate_next().unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                             store.release_storage(ticket).unwrap()
                                         }));
    assert!(panic.is_err());
    assert_eq!(owner.summary().pending, 1);
    assert!(store.state(owner.owner().incarnation()).is_none());
    let row =
        registry.by_id(registry.snapshot().unwrap().0, registry.get(STATE).unwrap().id()).unwrap();
    assert_eq!(row.uses, 1, "failed destruction cannot unpin its provider");
    // Host unwinding checks the resource barrier only, not native privileged recovery (M-D04).
}
