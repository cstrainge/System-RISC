use mac_object_table::{aliases::{self, ReleaseCause},
                       domain::{CapacityPolicy, CompletionError, DomainKind, KindCode,
                                OwnerIndex, ReleasedResource},
                       registry::{CleanupFailure, Registry, Release, Source, Specification},
                       shared::{InvocationGate, OwnerRef}};

struct Gate;
static CASES: std::sync::Mutex<()> = std::sync::Mutex::new(());
impl InvocationGate for Gate
{
    fn run<R>(f: impl FnOnce() -> R) -> R
    {
        f()
    }
}
fn owner(kind: DomainKind) -> OwnerRef<Gate>
{
    if kind == DomainKind::System
    {
        static SYSTEM: std::sync::OnceLock<OwnerRef<Gate>> = std::sync::OnceLock::new();
        return SYSTEM.get_or_init(|| OwnerRef::try_new(OwnerIndex::issue(kind).unwrap()).unwrap())
                     .clone();
    }
    OwnerRef::try_new(OwnerIndex::issue(kind).unwrap()).unwrap()
}
struct Retain;
impl Release<()> for Retain
{
    fn release(&self, _: &mut (), ticket: ReleasedResource) -> Result<(), CompletionError>
    {
        Err(CompletionError::registration_mismatch(ticket))
    }
}

#[test]
fn admission_receipts_cover_deferred_death_and_reject_a_reused_single_slot()
{
    let _serial = CASES.lock().unwrap();
    let system = owner(DomainKind::System);
    let app = owner(DomainKind::Process);
    let mut registry = Registry::new();
    let mut kind = |code, capacity| {
        registry.register(&system,
                          Specification { kind: KindCode(code),
                                          label: "Image receipt fixture",
                                          provider: "Fixture",
                                          capacity,
                                          source: Source::Resident },
                          Retain)
                .unwrap()
    };
    let a = kind(*b"IRAL", CapacityPolicy::PerDomainQuota { slots: 1 });
    let b = kind(*b"IRAS", CapacityPolicy::GrowableUntilMemory);
    let o = kind(*b"IRSO", CapacityPolicy::GrowableUntilMemory);
    let s = kind(*b"IRSS", CapacityPolicy::GrowableUntilMemory);
    let mut aliases = aliases::Store::new(system.clone(), (a, b), (o, s)).unwrap();
    let (first, receipt) = aliases.insert_with_receipt(&app, "source", 10).unwrap().into_parts();
    assert_eq!(receipt.owner(), app.owner());
    assert_eq!(aliases.completing(&receipt), Some((&"source", &10)));
    let mut alias_ticket = None;
    let mut backing_tickets = Vec::new();
    while let Some(ticket) = app.invalidate_next()
    {
        if ticket.key().kind() == KindCode(*b"IRAL")
        {
            alias_ticket = Some(ticket);
        }
        else
        {
            backing_tickets.push(ticket);
        }
    }
    let ticket = alias_ticket.expect("fixture admitted one real alias");
    assert!(aliases.get_with_owner(first).is_none());
    assert_eq!(aliases.completing(&receipt), Some((&"source", &10)));
    aliases.defer_alias(ticket).unwrap();
    assert_eq!(aliases.completing(&receipt), Some((&"source", &10)));
    assert_eq!(aliases.retained().count(), 1);
    assert_eq!(aliases.deferred_tags().copied().collect::<Vec<_>>(), vec![10]);
    let refused = aliases.insert(&app, "premature reuse", 20).unwrap_err();
    assert!(matches!(refused, aliases::Error::Pool(_)));
    assert!(refused.to_string().contains("IRAL"));
    assert_eq!(aliases.finish_deferred(|_| true), 1);
    for ticket in backing_tickets
    {
        aliases.release_alias_storage(ticket).unwrap();
    }
    assert!(aliases.completing(&receipt).is_none());
    let (second, next) = aliases.insert_with_receipt(&app, "replacement", 30).unwrap().into_parts();
    assert_ne!(first, second);
    assert!(aliases.completing(&receipt).is_none());
    assert_eq!(aliases.completing(&next), Some((&"replacement", &30)));
    let mut unrelated = mac_object_table::store::Store::new(KindCode(*b"WRNG"),
                                                            CapacityPolicy::GrowableUntilMemory);
    let (_, foreign) = unrelated.insert_with_receipt(&app, |_| 0).unwrap().into_parts();
    assert!(aliases.completing(&foreign).is_none());
    aliases.remove(&app, second).unwrap();
}
#[test]
fn shared_objects_are_generation_checked_and_retained_through_each_alias_death()
{
    let _serial = CASES.lock().unwrap();
    let system = owner(DomainKind::System);
    let registrar = owner(DomainKind::Module);
    let source = owner(DomainKind::Process);
    let receiver = owner(DomainKind::Process);
    let stopped = owner(DomainKind::Process);
    let mut registry = Registry::new();
    let mut kind = |code| {
        registry.register(&registrar,
                          Specification { kind:     KindCode(code),
                                          label:    "Shared fixture",
                                          provider: "Fixture",
                                          capacity: CapacityPolicy::GrowableUntilMemory,
                                          source:   Source::Resident },
                          Retain)
                .unwrap()
    };
    let a = kind(*b"ALIS");
    let b = kind(*b"ALST");
    let o = kind(*b"OBJT");
    let s = kind(*b"OBST");
    assert!(matches!(aliases::Store::<u32, Gate>::new(source.clone(),
                                                      (a.clone(), b.clone()),
                                                      (o.clone(), s.clone())),
                     Err(aliases::Error::NotSystem { .. })));
    let mut aliases = aliases::Store::new(system.clone(), (a, b), (o, s)).unwrap();
    let first = aliases.insert(&source, String::from("shared"), 10).unwrap();
    assert_eq!(aliases.get(&source, first).unwrap(), (&String::from("shared"), &10));
    let error = aliases.get(&receiver, first).unwrap_err();
    assert!(matches!(error, aliases::Error::Foreign { .. }));
    assert!(error.to_string().contains("requesting"));
    stopped.stop_admission();
    assert!(matches!(aliases.share(&source, first, &stopped, 11),
                     Err(aliases::Error::Pool(mac_object_table::domain::PoolError::Stopping { .. }))));
    let second = aliases.share(&source, first, &receiver, 20).unwrap();
    let third = aliases.insert(&receiver, String::from("shared"), 30).unwrap();
    assert_ne!(first, second);
    assert_eq!(aliases.objects().count(), 1);
    assert_eq!(aliases.iter().count(), 3);
    source.stop_admission();
    let mut tickets = Vec::new();
    while let Some(ticket) = source.invalidate_next()
    {
        tickets.push(ticket);
    }
    assert!(matches!(aliases.get(&source, first), Err(aliases::Error::Missing { .. })));
    assert_eq!(aliases.objects().count(), 1, "invalidation is not a release acknowledgement");
    for ticket in tickets
    {
        match ticket.key().kind().0
        {
            code if code == *b"ALIS" =>
            {
                let error = aliases.release_object(ticket).unwrap_err();
                let (ticket, cause) = error.into_parts();
                assert!(matches!(cause, ReleaseCause::Facility(_)));
                aliases.defer_alias(ticket).unwrap();
            }
            code if code == *b"ALST" => aliases.release_alias_storage(ticket).unwrap(),
            _ => panic!("unexpected cleanup kind")
        }
    }
    assert_eq!(source.summary().pending,
               1,
               "deferred alias was acknowledged before protocol completion");
    assert_eq!(aliases.finish_deferred(|_| false), 0);
    assert_eq!(source.summary().pending, 1);
    assert_eq!(aliases.objects().count(), 1);
    assert_eq!(aliases.finish_deferred(|_| true), 1);
    assert_eq!(source.summary().pending, 0);
    assert_eq!(aliases.get(&receiver, second).unwrap().0, "shared");
    aliases.remove(&receiver, second).unwrap();
    let replacement = aliases.insert(&receiver, String::from("other"), 40).unwrap();
    assert_ne!(replacement, second);
    assert!(matches!(aliases.get(&receiver, second), Err(aliases::Error::Missing { .. })));
    aliases.remove(&receiver, replacement).unwrap();
    assert_eq!(aliases.objects().count(), 1);
    // The System may invalidate a shared target first. Its ticket must remain
    // owned and pending while any process aliases refer to the retained value.
    let mut system_tickets = Vec::new();
    while let Some(ticket) = system.invalidate_next()
    {
        system_tickets.push(ticket);
    }
    let mut held = None;
    for ticket in system_tickets
    {
        match ticket.key().kind().0
        {
            code if code == *b"OBJT" =>
            {
                let error = aliases.release_object(ticket).unwrap_err();
                assert!(error.to_string().contains("1 aliases"));
                let (ticket, cause) = error.into_parts();
                assert_eq!(cause, ReleaseCause::Referenced { aliases: 1 });
                held = Some(ticket);
            }
            code if code == *b"OBST" => aliases.release_object_storage(ticket).unwrap(),
            _ => panic!("unexpected System kind")
        }
    }
    assert!(matches!(aliases.get(&receiver, third), Err(aliases::Error::TargetGone { .. })));
    aliases.remove(&receiver, third).unwrap();
    aliases.release_object(held.unwrap()).unwrap();
    assert_eq!(system.summary().pending, 0);
    assert_eq!(aliases.objects().count(), 0);
    drop(aliases);
    assert_eq!((receiver.summary().live, receiver.summary().pending), (0, 0));

    let backing = kind(*b"QBST");
    let object = kind(*b"QOBJ");
    let object_backing = kind(*b"QOST");
    let limited = registry.register(&registrar,
                                    Specification { kind:     KindCode(*b"QUOT"),
                                                    label:    "Limited aliases",
                                                    provider: "Fixture",
                                                    capacity:
                                                        CapacityPolicy::PerDomainQuota { slots: 1 },
                                                    source:   Source::Resident },
                                    Retain)
                          .unwrap();
    let mut limited =
        aliases::Store::new(system.clone(), (limited, backing), (object, object_backing)).unwrap();
    let accepted = limited.insert(&receiver, "accepted", ()).unwrap();
    let error = limited.insert(&receiver, "unpublished", ()).unwrap_err();
    assert!(matches!(error, aliases::Error::Pool(_)));
    assert!(error.to_string().contains("QUOT"));
    assert_eq!(limited.objects().copied().collect::<Vec<_>>(), vec!["accepted"]);
    assert_eq!(limited.get(&receiver, accepted).unwrap().0, &"accepted");
    let error = limited.share(&receiver, accepted, &receiver, ()).unwrap_err();
    assert!(error.to_string().contains("QUOT"));
    limited.remove(&receiver, accepted).unwrap();
    assert_eq!(limited.objects().count(), 0, "refused sharing added a reference");
    // The one-slot policy forces reuse of that exact owner slot. Identity and
    // generation validation must still reject the disposed alias.
    let replacement = limited.insert(&receiver, "replacement", ()).unwrap();
    assert_ne!(accepted, replacement);
    assert!(matches!(limited.get(&receiver, accepted), Err(aliases::Error::Missing { .. })));
    assert_eq!(limited.get(&receiver, replacement).unwrap().0, &"replacement");
    limited.remove(&receiver, replacement).unwrap();
}
