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
                          .admit(&Authority::Client(owner.clone()))
                          .unwrap();
    (registry, owner, subject, interface)
}
fn begin(registry: &mut Fixture, subject: &Subject, interface: u64) -> u64
{
    registry.begin_scoped(subject, 0, interface, 7, 3, 64).unwrap()
}
fn access(subject: &Subject, ticket: u64) -> Access<'_>
{
    Access::Continuing { subject,
                         ticket }
}
#[test]
fn invalidated_request_rejects_ordinary_ids_and_requires_its_exact_original_subject()
{
    let (mut registry, owner, subject, interface) = fixture(false);
    let other = registry.fixture_operations
                        .as_ref()
                        .unwrap()
                        .admit(&Authority::Client(owner.clone()))
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
    let error = registry.begin_scoped(&subject, 0, interface, 7, 0, 0).unwrap_err();
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
