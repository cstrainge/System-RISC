use mac_object_table::{domain::{CapacityPolicy, CompletionError, DomainKind, KindCode,
                                OwnerIndex, ReleasedResource},
                       registry::{Registry, Release, Source, Specification},
                       shared::{InvocationGate, OwnerRef},
                       store::{Removed, Store},
                       walk};
struct Gate;
impl InvocationGate for Gate
{
    fn run<R>(operation: impl FnOnce() -> R) -> R
    {
        operation()
    }
}
const NODE: KindCode = KindCode(*b"NODE");
struct Node
{
    child: Option<u64>,
    name:  u8
}
struct Context
{
    objects:   Store<Node, Gate>,
    wrong:     Store<Node, Gate>,
    fail_slot: Option<usize>,
    destroyed: Vec<u8>
}
struct Cleanup;
impl Cleanup
{
    fn destroy(context: &mut Context, removed: Removed<Node, Gate>)
    {
        if let Some(child) = removed.child
        {
            if let Some(child) = context.objects.remove(child)
            {
                Self::destroy(context, child);
            }
        }
        context.destroyed.push(removed.name);
        drop(removed);
    }
}
impl Release<Context> for Cleanup
{
    fn release(&self, context: &mut Context, resource: ReleasedResource)
               -> Result<(), CompletionError>
    {
        if context.fail_slot == Some(resource.key().slot())
        {
            // Real checked refusal preserves the ticket, without touching the
            // correct payload or pretending cleanup succeeded.
            return context.wrong.release(resource).map(drop);
        }
        let removed = context.objects.release(resource)?;
        Self::destroy(context, removed);
        Ok(())
    }
}
fn owner(kind: DomainKind) -> OwnerRef<Gate>
{
    OwnerRef::try_new(OwnerIndex::issue(kind).unwrap()).unwrap()
}
fn setup() -> (Registry<Context, Gate>, OwnerRef<Gate>, OwnerRef<Gate>, Context)
{
    let registrar = owner(DomainKind::Module);
    let subject = owner(DomainKind::Process);
    let mut registry = Registry::new();
    let kind = registry.register(&registrar,
                                 Specification { kind:     NODE,
                                                 label:    "Test nodes",
                                                 provider: "Node provider",
                                                 capacity: CapacityPolicy::GrowableUntilMemory,
                                                 source:   Source::Resident },
                                 Cleanup)
                       .unwrap();
    let context = Context { objects:   Store::registered(kind),
                            wrong:     Store::new(KindCode(*b"OTHR"),
                                                  CapacityPolicy::GrowableUntilMemory),
                            fail_slot: None,
                            destroyed: Vec::new() };
    (registry, registrar, subject, context)
}
#[test]
fn cycles_and_parent_cleanup_compose_without_phases_or_a_manager_roll_call()
{
    let (mut registry, registrar, subject, mut context) = setup();
    let parent = context.objects
                        .insert(&subject,
                                Node { child: None,
                                       name:  1 })
                        .unwrap();
    let child = context.objects
                       .insert(&subject,
                               Node { child: Some(parent),
                                      name:  2 })
                       .unwrap();
    context.objects.get_mut(parent).unwrap().child = Some(child);
    let report = walk::drain(&mut context,
                             &subject,
                             |_, key| registry.dispatch(key.kind()),
                             |_, error| panic!("unexpected cleanup failure: {error}")).unwrap();
    assert_eq!((report.invalidated, report.released, report.quarantined), (1, 1, 0));
    assert_eq!(context.destroyed, [2, 1]);
    assert_eq!((subject.summary().live, subject.summary().pending, subject.summary().deaths),
               (0, 0, 2));
    assert!(!subject.accepting());
    assert!(context.objects.get(parent).is_none() && context.objects.get(child).is_none());
    drop(registry.unregister(&registrar, NODE).unwrap());
}
#[test]
fn failed_cleanup_is_retained_without_allocation_and_does_not_stop_other_entries()
{
    let (mut registry, registrar, subject, mut context) = setup();
    let first = context.objects
                       .insert(&subject,
                               Node { child: None,
                                      name:  1 })
                       .unwrap();
    context.objects
           .insert(&subject,
                   Node { child: None,
                          name:  2 })
           .unwrap();
    context.fail_slot = Some(context.objects.key(first).unwrap().slot());
    let mut errors = Vec::new();
    let report = walk::drain(&mut context,
                             &subject,
                             |_, key| registry.dispatch(key.kind()),
                             |key, error| errors.push((key, error.to_string()))).unwrap();
    assert_eq!((report.invalidated, report.released, report.quarantined), (2, 1, 1));
    assert_eq!(context.destroyed, [2]);
    assert_eq!((subject.summary().live, subject.summary().pending, subject.summary().quarantined),
               (0, 1, 1));
    assert!(errors[0].1.contains("NODE") && errors[0].1.contains("registered cleanup refused"));
    let row = registry.at(registry.snapshot().unwrap().0, 0).unwrap();
    assert_eq!(row.uses, 1, "failed destruction must pin its provider");
    let attempts = walk::retry(&mut context,
                               &subject,
                               |_, key| registry.dispatch(key.kind()),
                               |_, _| {}).unwrap();
    assert_eq!((attempts.released, attempts.quarantined),
               (0, 1),
               "retry must be bounded even if nothing recovers");
    context.fail_slot = None;
    let retry = walk::retry(&mut context,
                            &subject,
                            |_, key| registry.dispatch(key.kind()),
                            |_, error| panic!("retry failed: {error}")).unwrap();
    assert_eq!((retry.released, retry.quarantined), (1, 0));
    assert_eq!(context.destroyed, [2, 1]);
    assert_eq!((subject.summary().pending, subject.summary().quarantined), (0, 0));
    assert!(subject.resume_quarantined().is_none());
    drop(registry.unregister(&registrar, NODE).unwrap());
}
#[test]
fn quarantine_transfers_the_unique_ticket_and_foreign_owners_cannot_take_it()
{
    let subject = owner(DomainKind::Process);
    let foreign = owner(DomainKind::Process);
    subject.add_pool(NODE, CapacityPolicy::PerDomainQuota { slots: 1 }).unwrap();
    subject.grow(NODE, 1).unwrap();
    let key = subject.allocate(NODE).unwrap();
    let ticket = subject.invalidate(key).unwrap();
    let error = foreign.quarantine(ticket).unwrap_err();
    assert!(error.to_string().contains("foreign") || error.to_string().contains("expected"));
    subject.quarantine(error.into_ticket()).unwrap();
    assert!(foreign.resume_quarantined().is_none());
    let resumed = subject.resume_quarantined().unwrap();
    assert_eq!(resumed.key(), key);
    assert!(subject.resume_quarantined().is_none());
    assert_eq!((subject.summary().pending, subject.summary().quarantined), (1, 0));
    subject.complete_release(resumed).unwrap();
    let next = subject.allocate(NODE).unwrap();
    assert_eq!(next.slot(), key.slot());
    assert_ne!(next.generation(), key.generation());
    assert!(!subject.contains(key));
    subject.complete_release(subject.invalidate(next).unwrap()).unwrap();
}
#[test]
fn repeated_failures_rotate_across_slots_instead_of_starving_later_tickets()
{
    let subject = owner(DomainKind::Process);
    subject.add_pool(NODE, CapacityPolicy::GrowableUntilMemory).unwrap();
    subject.grow(NODE, 3).unwrap();
    for _ in 0..3
    {
        let key = subject.allocate(NODE).unwrap();
        subject.quarantine(subject.invalidate(key).unwrap()).unwrap();
    }
    let mut seen = Vec::new();
    for _ in 0..3
    {
        let ticket = subject.resume_quarantined().unwrap();
        seen.push(ticket.key().slot());
        subject.quarantine(ticket).unwrap();
    }
    seen.sort_unstable();
    assert_eq!(seen, [0, 1, 2]);
    while let Some(ticket) = subject.resume_quarantined()
    {
        subject.complete_release(ticket).unwrap();
    }
}

#[test]
fn manager_specific_typed_errors_cross_the_walk_without_becoming_pool_errors()
{
    use mac_object_table::registry::CleanupFailure;
    #[derive(Debug, PartialEq, Eq)]
    enum IoCause
    {
        Flush
        {
            device: &'static str
        },
        WrongRegistration
    }
    impl std::fmt::Display for IoCause
    {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result
        {
            match self
            {
                Self::Flush { device } =>
                {
                    write!(f, "cannot flush {device}; restore device access and retry")
                }
                Self::WrongRegistration =>
                {
                    f.write_str("cleanup registration does not match the resource")
                }
            }
        }
    }
    struct IoFailure
    {
        ticket: ReleasedResource,
        cause:  IoCause
    }
    impl CleanupFailure for IoFailure
    {
        type Cause = IoCause;

        fn into_parts(self) -> (ReleasedResource, IoCause)
        {
            (self.ticket, self.cause)
        }

        fn registration_mismatch(ticket: ReleasedResource) -> Self
        {
            Self { ticket,
                   cause: IoCause::WrongRegistration }
        }
    }
    struct IoContext;
    struct IoCleanup;
    impl Release<IoContext, IoFailure> for IoCleanup
    {
        fn release(&self, _: &mut IoContext, ticket: ReleasedResource) -> Result<(), IoFailure>
        {
            Err(IoFailure { ticket,
                            cause: IoCause::Flush { device: "test device" } })
        }
    }
    let registrar = owner(DomainKind::Module);
    let subject = owner(DomainKind::Process);
    let mut registry = Registry::<IoContext, Gate, IoFailure>::new();
    let kind = registry.register(&registrar,
                                 Specification { kind:     NODE,
                                                 label:    "I/O objects",
                                                 provider: "Test I/O provider",
                                                 capacity: CapacityPolicy::GrowableUntilMemory,
                                                 source:   Source::Resident },
                                 IoCleanup)
                       .unwrap();
    let mut objects = Store::registered(kind);
    objects.insert(&subject, 1).unwrap();
    let mut received = false;
    let report = walk::drain(&mut IoContext,
                             &subject,
                             |_, key| registry.dispatch(key.kind()),
                             |_, error| {
                                 match error
                                 {
                                     walk::Failure::Cleanup(IoCause::Flush { device }) =>
                                     {
                                         assert_eq!(*device, "test device");
                                         received = true;
                                     }
                                     _ => panic!("manager error identity was discarded: {error}")
                                 }
                                 assert!(error.to_string()
                                              .contains("restore device access and retry"));
                             }).unwrap();
    assert!(received);
    assert_eq!((report.released, report.quarantined), (0, 1));
    // Finish with the actual owned store to leave the test domain clean.
    drop(objects.release(subject.resume_quarantined().unwrap()).unwrap());
    drop(registry.unregister(&registrar, NODE).unwrap());
}
