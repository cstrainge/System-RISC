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
