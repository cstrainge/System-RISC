use alloc::format;

use mac_module::{ClassId, Offer, Target};
use mac_object_table::{domain::{DomainKind, OwnerIndex},
                       registry::CleanupFailure};

use super::*;
use crate::{drivers::{jobs::subjects::Subject, registry::fixture::Fixture},
            toolbox::handles::{OwnerRef, subject::Authority}};

fn fixture(serialized: bool) -> (Fixture, OwnerRef, Subject, u64)
{
    let mut registry = Fixture::new(Target::RVA23U64);
    let interface = registry.register_service(Offer { class:          ClassId(*b"TEST"),
                                                      version:        1,
                                                      request_schema: 1,
                                                      reply_schema:   1,
                                                      max_request:    64,
                                                      max_reply:      64,
                                                      flags:          Offer::KERNEL_ONLY
                                                                      | if serialized
                                                                      {
                                                                          Offer::SERIALIZED
                                                                      }
                                                                      else
                                                                      {
                                                                          0
                                                                      },
                                                      capabilities:   0 })
                            .unwrap();
    let owner = OwnerRef::try_new(OwnerIndex::issue(DomainKind::Process).unwrap()).unwrap();
    let subject = registry.fixture_operations
                          .as_ref()
                          .unwrap()
                          .admit_current(&Authority::Client(owner.clone()))
                          .unwrap();
    (registry, owner, subject, interface)
}
fn begin(registry: &mut Fixture, subject: &Subject, interface: u64) -> u64
{
    registry.begin_scoped(subject, 0, interface, 7, 3, 64, Origin::Direct).unwrap()
}
fn access(subject: &Subject, ticket: u64) -> Access<'_>
{
    Access::Continuing { subject,
                         ticket }
}

fn entry_fixture(serialized: bool) -> (Fixture, Subject, u64, u64, u64)
{
    use super::super::tests::{OFFER, metadata, stage};
    let mut registry = Fixture::new(Target::RVA23U64);
    let bytes = metadata(*b"cancel-provider!",
                         &[Offer { flags: if serialized { Offer::SERIALIZED } else { 0 },
                                   ..OFFER }],
                         &[]);
    let provider = stage(&mut registry, &bytes);
    registry.commit(provider).unwrap();
    let interface = registry.resolve(0, ClassId(*b"TEST"), 1).unwrap();
    let dependency = mac_module::Dependency { module:   [0; 16],
                                              class:    ClassId(*b"TEST"),
                                              version:  1,
                                              optional: false };
    let bytes = metadata(*b"cancel-consumer!", &[], &[dependency]);
    let consumer = stage(&mut registry, &bytes);
    let subject = registry.fixture_subject(consumer).unwrap();
    (registry, subject, provider, interface, consumer)
}

#[test]
fn active_broker_parent_is_authenticated_and_queued_work_starts_an_independent_frame()
{
    let (mut registry, owner, subject, interface) = fixture(false);
    let parent = begin(&mut registry, &subject, interface);
    assert_eq!(registry.request_invocation(0, access(&subject, parent)).err(),
               Some(Error::RequestParent));
    registry.enter_request(0, access(&subject, parent)).unwrap();
    let invoking = registry.request_invocation(0, access(&subject, parent)).unwrap();
    let child = registry.begin_scoped(&invoking, 0, interface, 7, 0, 16, Origin::Direct).unwrap();
    let at = registry.requests.at(child).unwrap();
    let receipt = registry.requests.slots[at].receipt;
    assert_eq!(registry.requests.store.completing(&receipt).unwrap().parent.unwrap().id(), parent);
    assert!(registry.requests.slots[at].cancellation.is_none(),
            "nested protocol was given a second cancellation owner");
    let error =
        registry.begin_scoped(&invoking, 0, interface, 7, 0, 16, Origin::Queued).unwrap_err();
    assert_eq!(error, Error::RequestParent);
    assert!(format!("{error}").contains("matching active parent invocation"));
    let other = registry.fixture_operations
                        .as_ref()
                        .unwrap()
                        .admit_current(&Authority::Client(owner))
                        .unwrap();
    let forged = other.in_request(invoking.current_request().unwrap());
    assert_eq!(registry.begin_scoped(&forged, 0, interface, 7, 0, 16, Origin::Direct),
               Err(Error::RequestParent));
    let queued = invoking.queued();
    let independent =
        registry.begin_scoped(&queued, 0, interface, 7, 0, 16, Origin::Queued).unwrap();
    let at = registry.requests.at(independent).unwrap();
    assert!(registry.requests.slots[at].cancellation.is_none(),
            "queue-owned request was given a second cancellation owner");
    registry.leave_request(0, access(&subject, parent)).unwrap();
    assert_eq!(registry.begin_scoped(&invoking, 0, interface, 7, 0, 16, Origin::Direct),
               Err(Error::RequestParent));
    registry.complete(0, access(&queued, independent), 0).unwrap();
    registry.complete(0, access(&invoking, child), 0).unwrap();
    registry.complete(0, access(&subject, parent), 0).unwrap();
}

#[test]
fn failed_entry_cancels_only_its_direct_root_after_invalidation_and_actual_ack_drains_the_owner()
{
    let (mut registry, subject, provider, interface, consumer) = entry_fixture(false);
    let owner = registry.module_ledger(consumer).unwrap();
    let root =
        registry.begin_scoped(&subject, consumer, interface, 7, 0, 16, Origin::Direct).unwrap();
    registry.enter_request(consumer, access(&subject, root)).unwrap();
    let invoking = registry.request_invocation(consumer, access(&subject, root)).unwrap();
    // A real nested provider call has this executor as caller, even though the
    // subject remains the failed consumer. Kernel-only resident resolution is
    // covered separately; this test verifies its provenance at the core store.
    let request = Request { module: 0,
                            interface,
                            caller: provider,
                            class: ClassId(*b"TEST"),
                            operation: 7,
                            max_reply: 16,
                            interface_key: registry.interfaces.key(interface).unwrap(),
                            offer: registry.interface(interface).unwrap().offer,
                            active: false,
                            origin: Origin::Direct };
    let child = registry.requests.insert(&invoking, request).unwrap();
    let queued =
        registry.begin_scoped(&subject.queued(), consumer, interface, 7, 0, 16, Origin::Queued)
                .unwrap();
    registry.leave_request(consumer, access(&subject, root)).unwrap();
    assert_eq!(registry.rollback(consumer), Err(Error::Busy));
    assert_eq!(registry.request(consumer, root), Err(Error::Stale));
    let plan = registry.next_cancellation().unwrap().unwrap();
    assert_eq!(plan.ticket(), root);
    assert!(registry.next_cancellation().unwrap().is_none(),
            "nested or queue-owned protocol was stolen");
    assert!(owner.summary().pending != 0);
    registry.requests.finish(access(&invoking, child)).unwrap();
    registry.complete(consumer, access(&subject, queued), 0).unwrap();
    drop(invoking);
    drop(subject);
    registry.acknowledge_cancellation(plan, mac_abi::OPERATION_CANCELLED, 0).unwrap();
    registry.rollback(consumer).unwrap();
    assert_eq!(registry.state(consumer), Err(Error::Stale));
    assert_eq!(registry.modules.get(provider).unwrap().requests, 0);
    assert_eq!((owner.summary().live, owner.summary().pending), (0, 0));
}

#[test]
fn cancellation_refusal_returns_the_original_plan_and_no_reply_is_accepted_before_validation()
{
    let (mut registry, subject, _, interface, consumer) = entry_fixture(false);
    let ticket =
        registry.begin_scoped(&subject, consumer, interface, 7, 0, 16, Origin::Direct).unwrap();
    drop(subject);
    assert_eq!(registry.rollback(consumer), Err(Error::Busy));
    let plan = registry.next_cancellation().unwrap().unwrap();
    let (mut foreign, _, _, _, _) = entry_fixture(false);
    let refusal = foreign.return_cancellation(plan).unwrap_err();
    assert!(format!("{refusal}").contains("original plan remains owned"));
    assert_eq!(refusal.plan.ticket(), ticket);
    let refusal = registry.acknowledge_cancellation(refusal.plan, 0, 17).unwrap_err();
    assert_eq!(refusal.cause, Error::Contract(mac_module::Error::Limit));
    registry.return_cancellation(refusal.plan).unwrap();
    let plan = registry.next_cancellation().unwrap().unwrap();
    assert_eq!(plan.ticket(), ticket);
    registry.acknowledge_cancellation(plan, mac_abi::OPERATION_CANCELLED, 0).unwrap();
    registry.rollback(consumer).unwrap();
}

#[test]
fn lost_or_uncertain_cancellation_keeps_the_original_reply_reservation_and_provider_pin()
{
    let (mut registry, subject, provider, interface, consumer) = entry_fixture(true);
    let owner = registry.module_ledger(consumer).unwrap();
    let ticket =
        registry.begin_scoped(&subject, consumer, interface, 7, 0, 16, Origin::Direct).unwrap();
    drop(subject);
    assert_eq!(registry.rollback(consumer), Err(Error::Busy));
    let mut plan = registry.next_cancellation().unwrap().unwrap();
    plan.output[..8].copy_from_slice(b"maybe???");
    registry.retain_uncertain_cancellation(plan, mac_abi::BAD_ABI).unwrap();
    let at = registry.requests.at(ticket).unwrap();
    assert_eq!(&registry.requests.slots[at].cancellation
                                           .as_ref()
                                           .unwrap()
                                           .output
                                           .as_ref()
                                           .unwrap()[..8],
               b"maybe???");
    assert!(registry.next_cancellation().unwrap().is_none());
    assert!(owner.summary().pending != 0);
    assert_eq!(registry.modules.get(provider).unwrap().requests, 1);
    let new = registry.fixture_subject(0).unwrap();
    assert_eq!(registry.begin_scoped(&new, 0, interface, 7, 0, 16, Origin::Queued),
               Err(Error::Busy));
    let (mut other, subject, _, interface, consumer) = entry_fixture(false);
    let other_owner = other.module_ledger(consumer).unwrap();
    let ticket =
        other.begin_scoped(&subject, consumer, interface, 7, 0, 16, Origin::Direct).unwrap();
    drop(subject);
    other.requests.cancel_entry(consumer, other_owner.owner());
    let plan = other.next_cancellation().unwrap().unwrap();
    drop(plan);
    assert!(other.requests.slots[other.requests.at(ticket).unwrap()].uncertainty().is_some());
    assert!(other.next_cancellation().unwrap().is_none());
    other_owner.stop_admission();
    let key = other.requests.store.key(ticket).unwrap();
    let failure = other.requests.release(other_owner.invalidate(key).unwrap(), false).unwrap_err();
    let detail = format!("{failure}");
    assert!(detail.contains("uncertain cancellation result")
            && detail.contains("no terminal acknowledgement is proven"),
            "{detail}");
    other_owner.quarantine(failure.into_parts().0).unwrap();
}

#[test]
fn committed_cancellation_reply_is_retained_while_serialized_admission_is_released()
{
    let (mut registry, subject, provider, interface, consumer) = entry_fixture(true);
    let owner = registry.module_ledger(consumer).unwrap();
    let ticket =
        registry.begin_scoped(&subject, consumer, interface, 7, 0, 16, Origin::Direct).unwrap();
    drop(subject);
    registry.requests.cancel_entry(consumer, owner.owner());
    let mut plan = registry.next_cancellation().unwrap().unwrap();
    plan.output[..8].copy_from_slice(b"COMMIT!!");
    assert_eq!(registry.acknowledge_cancellation(plan, 0, 8).unwrap(),
               cancellation::Acknowledged::ReplyRetained { bytes:  8,
                                                           status: 0 });
    assert_eq!(registry.modules.get(provider).unwrap().requests, 0);
    let at = registry.requests.at(ticket).unwrap();
    assert_eq!(registry.requests.slots[at].reply.as_ref().unwrap().bytes, b"COMMIT!!");
    owner.stop_admission();
    let key = registry.requests.store.key(ticket).unwrap();
    let failure = registry.requests.release(owner.invalidate(key).unwrap(), false).unwrap_err();
    let detail = format!("{failure}");
    assert!(detail.contains("acknowledged reply bytes")
            && detail.contains("require class recovery")
            && detail.contains("unique cleanup ticket remain retained"),
            "{detail}");
    owner.quarantine(failure.into_parts().0).unwrap();
    assert!(owner.summary().pending != 0 && registry.next_cancellation().unwrap().is_none());
    let new = registry.fixture_subject(0).unwrap();
    let next = registry.begin_scoped(&new, 0, interface, 7, 0, 16, Origin::Queued).unwrap();
    registry.complete(0, access(&new, next), 0).unwrap();
    assert!(registry.rollback(consumer).is_err());
}

#[test]
fn direct_cancel_returns_are_preallocated_fifo_and_grow_past_old_limits()
{
    let (mut registry, subject, _, interface, consumer) = entry_fixture(false);
    let mut tickets = Vec::new();
    for _ in 0..320
    {
        tickets.push(registry.begin_scoped(&subject,
                                           consumer,
                                           interface,
                                           7,
                                           0,
                                           16,
                                           Origin::Direct)
                             .unwrap());
    }
    let capacity = registry.requests.cancel_ready.capacity();
    drop(subject);
    assert_eq!(registry.rollback(consumer), Err(Error::Busy));
    let first = registry.next_cancellation().unwrap().unwrap();
    let id = first.ticket();
    registry.return_cancellation(first).unwrap();
    let second = registry.next_cancellation().unwrap().unwrap();
    assert_ne!(second.ticket(), id, "one pending cancellation starved another root");
    registry.acknowledge_cancellation(second, mac_abi::OPERATION_CANCELLED, 0).unwrap();
    for _ in 0..319
    {
        let plan = registry.next_cancellation().unwrap().unwrap();
        registry.acknowledge_cancellation(plan, mac_abi::OPERATION_CANCELLED, 0).unwrap();
    }
    assert!(registry.next_cancellation().unwrap().is_none());
    assert_eq!(registry.requests.cancel_ready.capacity(), capacity);
    registry.rollback(consumer).unwrap();
    for id in tickets
    {
        assert_eq!(registry.request(consumer, id), Err(Error::Stale));
    }
}
#[test]
fn invalidated_request_rejects_ordinary_ids_and_requires_its_exact_original_subject()
{
    let (mut registry, owner, subject, interface) = fixture(false);
    let other = registry.fixture_operations
                        .as_ref()
                        .unwrap()
                        .admit_current(&Authority::Client(owner.clone()))
                        .unwrap();
    let ticket = begin(&mut registry, &subject, interface);
    assert!(ticket > u64::from(u32::MAX), "native broker ticket was narrowed");
    let key = registry.requests.store.key(ticket).unwrap();
    owner.stop_admission();
    let cleanup = owner.invalidate(key).unwrap();
    registry.requests.release(cleanup, false).unwrap();
    assert_eq!(registry.request(0, ticket), Err(Error::Stale));
    let error = registry.request(0, access(&other, ticket)).unwrap_err();
    assert_eq!(error, Error::RequestSubject);
    assert!(format!("{error}").contains("original admitted operation"));
    assert_eq!(registry.request(9, access(&subject, ticket)), Err(Error::Access));
    registry.enter_request(0, access(&subject, ticket)).unwrap();
    assert_eq!(registry.complete(0, access(&subject, ticket), 0), Err(Error::Busy));
    registry.leave_request(0, access(&subject, ticket)).unwrap();
    assert_eq!(registry.complete(0, access(&subject, ticket), 65),
               Err(Error::Contract(mac_module::Error::Limit)));
    assert!(owner.pending(key), "a refusal must not acknowledge pending I/O");
    registry.complete(0, access(&subject, ticket), 3).unwrap();
    assert!(!owner.pending(key));
    assert_eq!(registry.request(0, access(&subject, ticket)), Err(Error::Stale));
    assert!(subject.scope().admitting().is_ok());
}
#[test]
fn terminal_before_cleanup_dispatch_waits_for_the_unique_ticket()
{
    let (mut registry, owner, subject, interface) = fixture(false);
    let ticket = begin(&mut registry, &subject, interface);
    let key = registry.requests.store.key(ticket).unwrap();
    let cleanup = owner.invalidate(key).unwrap();
    registry.complete(0, access(&subject, ticket), 0).unwrap();
    assert!(owner.pending(key));
    assert_eq!(registry.request(0, access(&subject, ticket)), Err(Error::Stale));
    assert_eq!(registry.requests.iter().count(), 1, "interface pin retired before dispatcher");
    registry.requests.release(cleanup, false).unwrap();
    assert!(!owner.pending(key));
    assert_eq!(registry.requests.iter().count(), 0);
}
#[test]
fn request_and_backing_remain_retained_through_invalidation_and_new_scoped_admission()
{
    let (mut registry, owner, subject, interface) = fixture(false);
    let ticket = begin(&mut registry, &subject, interface);
    let key = registry.requests.store.key(ticket).unwrap();
    owner.stop_admission();
    let cleanup = owner.invalidate(key).unwrap();
    registry.requests.release(cleanup, false).unwrap();
    // This is an admitted operation's post-stop child, not a new client root.
    let followup = begin(&mut registry, &subject, interface);
    assert_ne!(followup, ticket);
    registry.complete(0, access(&subject, followup), 0).unwrap();
    registry.complete(0, access(&subject, ticket), 0).unwrap();
    assert!(!owner.pending(key));
    let final_child = begin(&mut registry, &subject, interface);
    registry.complete(0, access(&subject, final_child), 0).unwrap();
    assert_eq!(registry.requests.iter().count(), 0);
}
#[test]
fn serialized_offer_remains_busy_until_actual_terminal_reply_after_invalidation()
{
    let (mut registry, owner, subject, interface) = fixture(true);
    let ticket = begin(&mut registry, &subject, interface);
    let key = registry.requests.store.key(ticket).unwrap();
    registry.requests.release(owner.invalidate(key).unwrap(), false).unwrap();
    let error = registry.begin_scoped(&subject, 0, interface, 7, 0, 0, Origin::Direct).unwrap_err();
    assert_eq!(error, Error::Busy);
    assert!(format!("{error}").contains("actual completion"));
    registry.complete(0, access(&subject, ticket), 0).unwrap();
    let next = begin(&mut registry, &subject, interface);
    registry.complete(0, access(&subject, next), 0).unwrap();
}
#[test]
fn invalidated_interface_keeps_its_exact_request_pin_until_terminal_cleanup()
{
    let (mut registry, _owner, subject, interface) = fixture(false);
    let request = begin(&mut registry, &subject, interface);
    let key = registry.interfaces.key(interface).unwrap();
    let cleanup = registry.system.invalidate(key).unwrap();
    let failed = super::super::interfaces::ReleaseInterfaces(false).release(&mut *registry,
                                                                            cleanup)
                                                                   .unwrap_err();
    assert!(format!("{failed}").contains("actual completion"));
    assert_eq!(registry.interface(interface), Err(Error::Stale));
    assert_eq!(registry.interface_for_request(0, access(&subject, request)).unwrap().offer.class,
               ClassId(*b"TEST"));
    registry.complete(0, access(&subject, request), 0).unwrap();
    let (cleanup, _) = failed.into_parts();
    super::super::interfaces::ReleaseInterfaces(false).release(&mut *registry, cleanup).unwrap();
    assert!(!registry.system.pending(key));
}
#[test]
fn native_request_store_grows_past_old_projection_limit_and_old_ids_stay_rejected()
{
    let (mut registry, _owner, subject, interface) = fixture(false);
    let mut tickets = Vec::new();
    for _ in 0..300
    {
        tickets.push(begin(&mut registry, &subject, interface));
    }
    assert_eq!(registry.requests.iter().count(), 300);
    for &ticket in &tickets
    {
        registry.complete(0, access(&subject, ticket), 0).unwrap();
    }
    let replacement = begin(&mut registry, &subject, interface);
    for ticket in tickets
    {
        assert_ne!(replacement, ticket);
        assert_eq!(registry.request(0, ticket), Err(Error::Stale));
        assert_eq!(registry.request(0, access(&subject, ticket)), Err(Error::Stale));
    }
    registry.complete(0, access(&subject, replacement), 0).unwrap();
}
#[test]
fn foreign_unique_ticket_is_refused_without_acknowledging_either_store()
{
    let (mut first, a, subject_a, interface_a) = fixture(false);
    let (mut second, b, subject_b, interface_b) = fixture(false);
    let ia = begin(&mut first, &subject_a, interface_a);
    let ib = begin(&mut second, &subject_b, interface_b);
    let ka = first.requests.store.key(ia).unwrap();
    let kb = second.requests.store.key(ib).unwrap();
    let ta = a.invalidate(ka).unwrap();
    let tb = b.invalidate(kb).unwrap();
    let refused = first.requests.release(tb, false).unwrap_err();
    assert!(!format!("{refused}").is_empty());
    assert!(a.pending(ka) && b.pending(kb));
    second.requests.release(refused.into_parts().0, false).unwrap();
    first.requests.release(ta, false).unwrap();
    first.complete(0, access(&subject_a, ia), 0).unwrap();
    second.complete(0, access(&subject_b, ib), 0).unwrap();
    assert!(!a.pending(ka) && !b.pending(kb));
}
#[test]
fn refused_terminal_release_retains_typed_cause_and_retries_the_same_ticket()
{
    let (mut first, a, subject_a, interface_a) = fixture(false);
    let (mut second, b, subject_b, interface_b) = fixture(false);
    let ia = begin(&mut first, &subject_a, interface_a);
    let ib = begin(&mut second, &subject_b, interface_b);
    let ka = first.requests.store.key(ia).unwrap();
    let kb = second.requests.store.key(ib).unwrap();
    first.requests.release(a.invalidate(ka).unwrap(), false).unwrap();
    second.requests.release(b.invalidate(kb).unwrap(), false).unwrap();
    core::mem::swap(&mut first.requests.slots[0].pending, &mut second.requests.slots[0].pending);
    first.complete(0, access(&subject_a, ia), 0).unwrap();
    assert!(first.requests.slots[0].terminal);
    assert!(first.requests.slots[0].failure.is_some());
    assert!(a.pending(ka) && b.pending(kb));
    core::mem::swap(&mut first.requests.slots[0].pending, &mut second.requests.slots[0].pending);
    first.requests.retire(0);
    second.complete(0, access(&subject_b, ib), 0).unwrap();
    assert!(!a.pending(ka) && !b.pending(kb));
    assert_eq!(first.requests.slots.len(), 0);
}

#[test]
fn common_owner_walk_defers_request_and_storage_until_provider_ack_and_last_scope_use()
{
    use crate::toolbox::handles::cleanup::{Checkpoint, context::Context};
    let (mut registry, owner, subject, interface) = fixture(false);
    let ticket = begin(&mut registry, &subject, interface);
    let catalog = registry._kinds.clone();
    let checkpoint = Checkpoint::try_new().unwrap();
    let report = checkpoint.run(&owner, || {
                               mac_object_table::walk::drain(&mut *registry as &mut dyn Context,
                                                             &owner,
                                                             |_, key| catalog.dispatch(key.kind()),
                                                             |_, _| ())
                           })
                           .unwrap();
    assert!(report.deferred > 0);
    assert!(owner.summary().pending > 0);
    assert_eq!(registry.request(0, ticket), Err(Error::Stale));
    assert!(subject.scope().admitting().is_ok());
    registry.enter_request(0, access(&subject, ticket)).unwrap();
    registry.leave_request(0, access(&subject, ticket)).unwrap();
    registry.complete(0, access(&subject, ticket), 0).unwrap();
    drop(subject);
    checkpoint.run(&owner, || {
                  mac_object_table::walk::retry(&mut *registry as &mut dyn Context,
                                                &owner,
                                                |_, key| catalog.dispatch(key.kind()),
                                                |_, _| ())
              })
              .unwrap();
    let summary = owner.summary();
    assert_eq!(summary.live, 0);
    assert_eq!(summary.pending, 0);
    assert_eq!(summary.quarantined, 0);
}

#[test]
fn serialization_tracks_transport_ack_without_waiting_for_cleanup_dispatch()
{
    let (mut registry, owner, subject, interface) = fixture(true);
    let ticket = begin(&mut registry, &subject, interface);
    let key = registry.requests.store.key(ticket).unwrap();
    let cleanup = owner.invalidate(key).unwrap();
    registry.complete(0, access(&subject, ticket), 0).unwrap();
    assert!(owner.pending(key));
    assert!(!registry.requests.busy(interface), "actual provider work already completed");
    let next = begin(&mut registry, &subject, interface);
    registry.complete(0, access(&subject, next), 0).unwrap();
    registry.requests.release(cleanup, false).unwrap();
    assert_eq!(registry.requests.iter().count(), 0);
    assert!(!owner.pending(key));
}

#[test]
fn synchronous_admission_retains_parent_authentication_without_publishing_a_ticket()
{
    let (mut registry, owner, subject, interface) = fixture(false);
    let before = owner.summary();
    for _ in 0..3
    {
        registry.authorize_synchronous(&subject, 0, interface, 3, 64).unwrap();
    }
    assert!(registry.requests.slots.is_empty());
    assert_eq!(owner.summary().live, before.live);
    assert_eq!(registry.authorize_synchronous(&subject, 0, interface, 65, 64),
               Err(Error::Contract(mac_module::Error::Limit)));
    assert!(format!("{}", Error::Contract(mac_module::Error::Limit)).contains("rebuild"));
    let parent = begin(&mut registry, &subject, interface);
    registry.enter_request(0, access(&subject, parent)).unwrap();
    let invoking = registry.request_invocation(0, access(&subject, parent)).unwrap();
    registry.authorize_synchronous(&invoking, 0, interface, 0, 0).unwrap();
    let other = registry.fixture_operations
                        .as_ref()
                        .unwrap()
                        .admit_current(&Authority::Client(owner))
                        .unwrap();
    let forged = other.in_request(invoking.current_request().unwrap());
    assert_eq!(registry.authorize_synchronous(&forged, 0, interface, 0, 0),
               Err(Error::RequestParent));
    registry.leave_request(0, access(&subject, parent)).unwrap();
    assert_eq!(registry.authorize_synchronous(&invoking, 0, interface, 0, 0),
               Err(Error::RequestParent));
    registry.complete(0, access(&subject, parent), 0).unwrap();
    let (registry, subject, _, interface, consumer) = entry_fixture(false);
    assert_eq!(registry.authorize_synchronous(&subject, consumer, interface, 0, 16),
               Err(Error::Access));
    assert!(format!("{}", Error::Access).contains("does not authorize"));
}

#[test]
fn synchronous_leaf_rejects_an_inherited_closed_root_but_keeps_stopped_open_continuations()
{
    let (registry, owner, external, interface) = fixture(false);
    let operations = registry.fixture_operations.as_ref().unwrap();
    let inherited =
        operations.admit_current(&Authority::Operation(external.scope().clone())).unwrap();
    owner.stop_admission();
    assert!(!owner.accepting());
    assert!(!inherited.scope().closed());
    let root = inherited.scope().root().unwrap();
    let pending = owner.invalidate(root).unwrap();
    assert!(owner.pending(root));
    // The real operation manager consumes the original cleanup ticket, retaining
    // its removed root through the external use instead of fabricating a pin.
    operations.release(pending, false).unwrap();
    assert!(owner.pending(root));
    let mut effects = 0;
    registry.authorize_synchronous(&inherited, 0, interface, 0, 0).unwrap();
    effects += 1;
    assert_eq!(effects, 1, "open external scope must retain continuation authority");
    drop(external);
    assert!(inherited.scope().closed());
    effects = 0;
    let before = owner.summary();
    let cause = registry.authorize_synchronous(&inherited, 0, interface, 0, 0).unwrap_err();
    if registry.authorize_synchronous(&inherited, 0, interface, 0, 0).is_ok()
    {
        effects += 1;
    }
    assert_eq!(effects, 0, "closed scope ran a device/service effect");
    assert_eq!(owner.summary(), before, "refused scope changed its ledger");
    assert!(
            matches!(cause, Error::Owner(mac_object_table::domain::PoolError::Scope {
        cause: mac_object_table::scope::Failure::Closed, ..
    })),
            "{cause:?}"
    );
    let message = format!("{cause}");
    assert!(message.contains("resource scope is closed")
            && message.contains("no new child operation was admitted"),
            "{message}");
    assert!(registry.requests.slots.is_empty(),
            "synchronous refusal published a hidden retry ticket");
}
