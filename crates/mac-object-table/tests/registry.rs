use mac_object_table::{domain::{CapacityPolicy, CompletionError, Directory, DomainKind, KindCode,
                                OwnerIndex, PoolError, ReleasedResource},
                       registry::{Error, Registry, Release, Source, Specification},
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
struct Context
{
    objects:  Store<u32, Gate>,
    released: usize
}
struct Cleanup;
impl Release<Context> for Cleanup
{
    fn release(&self, context: &mut Context, resource: ReleasedResource)
               -> Result<(), CompletionError>
    {
        let removed = context.objects.release(resource)?;
        drop(removed);
        context.released += 1;
        Ok(())
    }
}
fn owner(kind: DomainKind) -> OwnerRef<Gate>
{
    OwnerRef::try_new(OwnerIndex::issue(kind).unwrap()).unwrap()
}
fn spec() -> Specification<'static>
{
    Specification { kind:     KIND,
                    label:    "Managed controls",
                    provider: "System Control Manager",
                    capacity: CapacityPolicy::GrowableUntilMemory,
                    source:   Source::BundleResource { bundle: *b"SystemRISC-Kinds",
                                                       kind:   KindCode(*b"KIND"),
                                                       id:     1 } }
}
#[test]
fn registrations_are_owned_named_revisioned_and_carry_real_source_evidence()
{
    let registrar = owner(DomainKind::Module);
    let mut registry = Registry::<Context, Gate>::new();
    let before = registry.snapshot().unwrap();
    let mut reserved = spec();
    reserved.kind = mac_object_table::registry::REGISTRATION_KIND;
    let error = registry.register(&registrar, reserved, Cleanup).err().unwrap();
    assert!(matches!(error, Error::ReservedNamespace { .. }));
    assert!(error.to_string().contains("bootstrap registrations")
            && error.to_string().contains("choose a separate object namespace"));
    assert_eq!(registry.snapshot().unwrap(), before);
    assert_eq!(registrar.summary().live, 0);
    let kind = registry.register(&registrar, spec(), Cleanup).unwrap();
    assert_eq!((registrar.summary().live, registrar.summary().births), (1, 1));
    assert!(registrar.contains(kind.registration().release.registration));
    assert!(matches!(registry.at(before.0, 0), Err(Error::SnapshotStale { .. })));
    let (revision, count) = registry.snapshot().unwrap();
    assert_eq!(count, 1);
    let row = registry.at(revision, 0).unwrap();
    assert_eq!(row.label.as_str(), "Managed controls");
    assert_eq!(row.provider.as_str(), "System Control Manager");
    assert_eq!(row.source, spec().source);
    assert_eq!(row.registration.release.registration.owner(), registrar.owner());
    assert_eq!(row.uses, 0);
    assert!(row.admitting);
    let error = registry.register(&registrar, spec(), Cleanup).err().unwrap();
    assert!(matches!(error, Error::Duplicate { .. }));
    assert!(error.to_string().contains("System Control Manager"));
    assert_eq!(registry.snapshot().unwrap(), (revision, count));
    let mut bad = spec();
    bad.provider = "\n";
    assert!(matches!(registry.register(&registrar, bad, Cleanup),
                     Err(Error::Label { field: "provider name",
                                        .. })));
    drop(registry.unregister(&registrar, KIND).unwrap());
    assert!(matches!(registry.get(KIND), Err(Error::Missing { .. })));
    assert_eq!(registrar.summary().live, 0);
}
#[test]
fn busy_unregister_stops_admission_but_preserves_cleanup_and_old_tokens_never_revive()
{
    let registrar = owner(DomainKind::Module);
    let subject = owner(DomainKind::Process);
    let mut registry = Registry::<Context, Gate>::new();
    let lease = registry.register(&registrar, spec(), Cleanup).unwrap();
    let mut context = Context { objects:  Store::registered(lease.clone()),
                                released: 0 };
    let id = context.objects.insert(&subject, 7).unwrap();
    let ticket = subject.invalidate_next().unwrap();
    assert_eq!(ticket.key().registration(), Some(lease.id()));
    assert!(context.objects.get(id).is_none());
    let invocation = registry.dispatch(KIND).unwrap();
    let error = registry.unregister(&registrar, KIND).unwrap_err();
    assert!(matches!(error,
                     Error::Busy { remaining: 2,
                                   .. }));
    assert!(error.to_string().contains("2 resources or invocations still require cleanup"));
    assert_eq!(registrar.summary().live, 1);
    assert!(!registry.at(registry.snapshot().unwrap().0, 0).unwrap().admitting);
    assert!(matches!(context.objects.insert(&subject, 8),
                     Err(PoolError::RegistrationUnavailable { .. })));
    invocation.invoke(&mut context, ticket).unwrap();
    assert_eq!(context.released, 1);
    assert_eq!((subject.summary().live, subject.summary().pending), (0, 0));
    drop(registry.unregister(&registrar, KIND).unwrap());
    assert!(context.objects.insert(&subject, 9).is_err());
    drop(context);
    let new = registry.register(&registrar, spec(), Cleanup).unwrap();
    assert_ne!(lease.id(), new.id());
    let mut objects = Store::registered(new);
    let fresh = objects.insert(&subject, 10).unwrap();
    assert_ne!(id, fresh);
    assert_eq!(objects.get(id), None);
    assert!(subject.summary().slots == 1, "registration replacement preserves slot history");
    drop(objects);
    drop(registry.unregister(&registrar, KIND).unwrap());
}
#[test]
fn foreign_registrations_cannot_dispatch_an_equal_kind_and_refusal_keeps_the_ticket()
{
    let registrar = owner(DomainKind::Module);
    let foreign = owner(DomainKind::Module);
    let subject = owner(DomainKind::Process);
    let mut right = Registry::<Context, Gate>::new();
    let mut wrong = Registry::<Context, Gate>::new();
    let lease = right.register(&registrar, spec(), Cleanup).unwrap();
    wrong.register(&foreign, spec(), Cleanup).unwrap();
    let mut context = Context { objects:  Store::registered(lease),
                                released: 0 };
    context.objects.insert(&subject, 42).unwrap();
    let ticket = subject.invalidate_next().unwrap();
    let error = wrong.dispatch(KIND).unwrap().invoke(&mut context, ticket).unwrap_err();
    assert!(error.to_string().contains("ticket remains owned for retry"));
    assert_eq!(context.released, 0);
    assert!(matches!(right.unregister(&foreign, KIND), Err(Error::Foreign { .. })));
    right.dispatch(KIND).unwrap().invoke(&mut context, error.into_ticket()).unwrap();
    drop(right.unregister(&registrar, KIND).unwrap());
    drop(wrong.unregister(&foreign, KIND).unwrap());
}
#[test]
fn dropping_a_busy_registry_quarantines_its_root_instead_of_faking_domain_cleanup()
{
    let mut directory = Directory::<(), Gate>::new();
    let id = directory.insert(OwnerRef::issue(DomainKind::Module).unwrap(), ()).unwrap();
    let registrar = directory.get(id.incarnation()).unwrap().ledger().clone();
    let subject = owner(DomainKind::Process);
    let mut registry = Registry::<Context, Gate>::new();
    let kind = registry.register(&registrar, spec(), Cleanup).unwrap();
    let mut objects = Store::registered(kind);
    objects.insert(&subject, 1).unwrap();
    drop(registry);
    assert_eq!((registrar.summary().live, registrar.summary().pending), (0, 1));
    assert!(directory.remove(id.incarnation())
                     .unwrap_err()
                     .to_string()
                     .contains("cleanup of 1 invalidated ledger resources is still pending"));
    assert!(matches!(objects.insert(&subject, 2), Err(PoolError::RegistrationUnavailable { .. })));
    drop(objects);
}

#[test]
fn unregister_releases_provider_state_after_the_host_registry_lock_is_gone()
{
    use std::sync::{Arc, Mutex, Weak,
                    atomic::{AtomicBool, Ordering}};
    struct Provider
    {
        registry: Weak<Mutex<Registry<Context, Gate>>>,
        dropped:  Arc<AtomicBool>
    }
    impl Release<Context> for Provider
    {
        fn release(&self, context: &mut Context, resource: ReleasedResource)
                   -> Result<(), CompletionError>
        {
            Cleanup.release(context, resource)
        }
    }
    impl Drop for Provider
    {
        fn drop(&mut self)
        {
            if let Some(registry) = self.registry.upgrade()
            {
                assert!(registry.try_lock().is_ok(),
                        "provider destructor ran under the registry lock");
            }
            self.dropped.store(true, Ordering::Release);
        }
    }
    let registry = Arc::new(Mutex::new(Registry::new()));
    let dropped = Arc::new(AtomicBool::new(false));
    let registrar = owner(DomainKind::Module);
    let kind = registry.lock()
                       .unwrap()
                       .register(&registrar,
                                 spec(),
                                 Provider { registry: Arc::downgrade(&registry),
                                            dropped:  dropped.clone() })
                       .unwrap();
    let cleanup = { registry.lock().unwrap().unregister(&registrar, KIND).unwrap() };
    assert!(!dropped.load(Ordering::Acquire));
    assert_eq!(registrar.summary().pending, 1);
    assert!(!registrar.contains(kind.registration().release.registration));
    drop(cleanup);
    assert!(dropped.load(Ordering::Acquire));
    assert_eq!(registrar.summary().pending, 0);
}

#[test]
fn admission_racing_unregister_cannot_publish_after_the_provider_was_freed()
{
    use std::sync::{Arc, Barrier, Mutex};
    for _ in 0..80
    {
        let registrar = owner(DomainKind::Module);
        let subject = owner(DomainKind::Process);
        let mut registry = Registry::<Context, Gate>::new();
        let kind = registry.register(&registrar, spec(), Cleanup).unwrap();
        let mut objects = Store::registered(kind);
        let registry = Arc::new(Mutex::new(registry));
        let start = Arc::new(Barrier::new(2));
        let worker_registry = registry.clone();
        let worker_start = start.clone();
        let worker_owner = registrar.clone();
        let worker = std::thread::spawn(move || {
            worker_start.wait();
            let result = { worker_registry.lock().unwrap().unregister(&worker_owner, KIND) };
            match result
            {
                Ok(cleanup) =>
                {
                    drop(cleanup);
                    true
                }
                Err(Error::Busy { .. }) => false,
                Err(error) => panic!("unexpected unregister failure: {error}")
            }
        });
        start.wait();
        let inserted = objects.insert(&subject, 17);
        let unregistered = worker.join().unwrap();
        if unregistered
        {
            assert!(inserted.is_err(), "resource published after successful unregister");
            assert_eq!(subject.summary().live, 0);
        }
        else
        {
            // An in-flight admission is also a counted use even if the subject
            // subsequently refuses it; retry after that use has completed.
            drop(objects);
            let cleanup = { registry.lock().unwrap().unregister(&registrar, KIND).unwrap() };
            drop(cleanup);
        }
        assert_eq!((registrar.summary().live, registrar.summary().pending), (0, 0));
    }
}

#[test]
fn panicking_payload_cleanup_keeps_the_provider_pin_and_owner_barrier()
{
    struct Bomb;
    impl Drop for Bomb
    {
        fn drop(&mut self)
        {
            panic!("contained host-only destructor probe");
        }
    }
    struct BombContext
    {
        objects: Store<Bomb, Gate>
    }
    struct BombCleanup;
    impl Release<BombContext> for BombCleanup
    {
        fn release(&self, context: &mut BombContext, resource: ReleasedResource)
                   -> Result<(), CompletionError>
        {
            drop(context.objects.release(resource)?);
            Ok(())
        }
    }
    let registrar = owner(DomainKind::Module);
    let subject = owner(DomainKind::Process);
    let mut registry = Registry::<BombContext, Gate>::new();
    let kind = registry.register(&registrar, spec(), BombCleanup).unwrap();
    let mut context = BombContext { objects: Store::registered(kind) };
    let id = context.objects.insert(&subject, Bomb).unwrap();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                         drop(context.objects.remove(id).unwrap());
                                     })).is_err());
    assert_eq!((subject.summary().live, subject.summary().pending), (0, 1));
    assert!(matches!(registry.unregister(&registrar, KIND),
                     Err(Error::Busy { remaining: 1,
                                       .. })));
    // This is a resource-lifetime proof under host unwind, never a claim of
    // privileged native fault recovery (M-D04).
}
