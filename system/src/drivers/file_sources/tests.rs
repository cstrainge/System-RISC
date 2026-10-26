use alloc::{format, vec::Vec};

use mac_object_table::{domain::{DomainKind, OwnerIndex},
                       registry::Release};

use super::*;
use crate::toolbox::handles::Registry;

const FILE: Identity = Identity { volume: 66513,
                                  cnid:   42,
                                  fork:   0 };
fn owner() -> OwnerRef
{
    OwnerRef::try_new(OwnerIndex::issue(DomainKind::Module).unwrap()).unwrap()
}
struct ReleaseSource(bool);
impl Release<Sources<u32>, Failure> for ReleaseSource
{
    fn release(&self, sources: &mut Sources<u32>, ticket: ReleasedResource) -> Result<(), Failure>
    {
        sources.release(ticket, self.0)
    }
}
fn fixture() -> (Sources<u32>, Registry<Sources<u32>>)
{
    let registrar = owner();
    let mut catalog = Registry::new();
    let source =
        catalog.register(&registrar, lifetime::specification(false), ReleaseSource(false)).unwrap();
    let backing =
        catalog.register(&registrar, lifetime::specification(true), ReleaseSource(true)).unwrap();
    (Sources::new((source, backing)).unwrap(), catalog)
}
fn drain(sources: &mut Sources<u32>, owner: &OwnerRef, catalog: &Registry<Sources<u32>>)
{
    mac_object_table::walk::drain(sources, owner, |_, key| catalog.dispatch(key.kind()), |_, _| ()).unwrap();
}
fn retry(sources: &mut Sources<u32>, owner: &OwnerRef, catalog: &Registry<Sources<u32>>)
{
    mac_object_table::walk::retry(sources, owner, |_, key| catalog.dispatch(key.kind()), |_, _| ()).unwrap();
}
#[test]
fn uncertain_read_preserves_its_owned_plan_until_real_acknowledgement()
{
    let (mut sources, _catalog) = fixture();
    let a = owner();
    let source = sources.publish(&a, FILE, &11, 20).unwrap();
    let read = sources.begin_read(&a, source, 88, 0, 10).unwrap();
    sources.quarantine_read(&read).unwrap();
    assert_eq!(sources.info(&a, source), Err(ROLLBACK_FAILED));
    assert_eq!(sources.find(&a, FILE), Err(ROLLBACK_FAILED));
    assert_eq!(sources.begin_close(&a, source).err(), Some(ROLLBACK_FAILED));
    sources.finish_read(read).unwrap();
    assert_eq!(sources.info(&a, source), Ok(Info { length: 20 }));
    let close = sources.begin_close(&a, source).unwrap();
    sources.finish_close(close).unwrap();
}
#[test]
fn native_source_reuse_preserves_identity_and_rejects_old_and_foreign_capabilities()
{
    let (mut sources, _catalog) = fixture();
    let a = owner();
    let b = owner();
    let source = sources.publish(&a, FILE, &123, 70_000).unwrap();
    assert!(source > u32::MAX as u64);
    assert_eq!(sources.find(&a, FILE), Ok(Some(source)));
    assert_eq!(sources.publish(&a, FILE, &124, 0), Err(ALREADY_EXISTS));
    assert_eq!(sources.info(&b, source), Err(ACCESS_DENIED));
    let read = sources.begin_read(&a, source, 1, 69_999, 8).unwrap();
    assert_eq!((read.backing, read.offset, read.length, read.ticket()), (123, 69_999, 1, 1));
    assert_eq!(sources.begin_close(&a, source).err(), Some(SERVICE_BUSY));
    assert_eq!(sources.begin_read(&a, source, 2, 0, 1).err(), Some(SERVICE_BUSY));
    sources.finish_read(read).unwrap();
    assert_eq!(sources.begin_read(&a, source, 2, 70_001, 0).err(), Some(BAD_ARGUMENT));
    let close = sources.begin_close(&a, source).unwrap();
    assert_eq!(close.backing, 123);
    sources.quarantine_close(&close).unwrap();
    assert_eq!(sources.find(&a, FILE), Err(ROLLBACK_FAILED));
    sources.finish_close(close).unwrap();
    let replacement = sources.publish(&a, FILE, &126, 0).unwrap();
    let key = sources.entries.key(replacement).unwrap();
    assert_eq!(key.slot(), 0, "the sole source slot must be reused");
    assert_ne!(source, replacement);
    assert_eq!(sources.info(&a, source), Err(NOT_FOUND));
    let (mut foreign, _catalog) = fixture();
    let other = foreign.publish(&b, FILE, &127, 0).unwrap();
    assert_ne!(other, replacement);
    assert_eq!(sources.info(&a, other), Err(NOT_FOUND));
    let close = sources.begin_close(&a, replacement).unwrap();
    sources.finish_close(close).unwrap();
    let close = foreign.begin_close(&b, other).unwrap();
    foreign.finish_close(close).unwrap();
}
#[test]
fn dropping_read_or_close_plans_never_reopens_or_reuses_uncertain_backing()
{
    let (mut sources, _catalog) = fixture();
    let a = owner();
    let first = sources.publish(&a, FILE, &1, 20).unwrap();
    let read = sources.begin_read(&a, first, 5, 0, 20).unwrap();
    drop(read);
    assert_eq!(sources.info(&a, first), Err(ROLLBACK_FAILED));
    assert_eq!(sources.begin_close(&a, first).err(), Some(ROLLBACK_FAILED));
    assert_eq!(sources.publish(&a, FILE, &2, 20), Err(ROLLBACK_FAILED));
    let other_file = Identity { cnid: 43,
                                ..FILE };
    let second = sources.publish(&a, other_file, &3, 20).unwrap();
    let close = sources.begin_close(&a, second).unwrap();
    drop(close);
    assert_eq!(sources.find(&a, other_file), Err(ROLLBACK_FAILED));
    assert_eq!(sources.begin_read(&a, second, 6, 0, 10).err(), Some(ROLLBACK_FAILED));
}
#[test]
fn foreign_completion_refusal_returns_original_plan_for_its_real_manager()
{
    let (mut sources, _catalog) = fixture();
    let (mut foreign, _catalog) = fixture();
    let a = owner();
    let id = sources.publish(&a, FILE, &1, 20).unwrap();
    let read = sources.begin_read(&a, id, 5, 0, 10).unwrap();
    let refused = foreign.finish_read(read).unwrap_err();
    assert_eq!(refused.cause, NOT_FOUND);
    assert!(format!("{refused}").contains("original plan remains owned"));
    assert_eq!(sources.begin_close(&a, id).err(), Some(SERVICE_BUSY));
    sources.finish_read(refused.plan).unwrap();
    let close = sources.begin_close(&a, id).unwrap();
    let refused = foreign.finish_close(close).unwrap_err();
    assert_eq!(refused.cause, NOT_FOUND);
    sources.finish_close(refused.plan).unwrap();
    assert_eq!(sources.info(&a, id), Err(NOT_FOUND));
}
#[test]
fn common_walk_waits_for_read_then_queues_exactly_one_owned_close_and_real_ack()
{
    let (mut sources, catalog) = fixture();
    let a = owner();
    let id = sources.publish(&a, FILE, &9, 20).unwrap();
    let key = sources.entries.key(id).unwrap();
    let read = sources.begin_read(&a, id, 99, 0, 10).unwrap();
    drain(&mut sources, &a, &catalog);
    assert!(a.pending(key));
    assert_eq!(sources.info(&a, id), Err(NOT_FOUND));
    assert!(sources.next_cleanup().unwrap().is_none());
    sources.finish_read(read).unwrap();
    let close = sources.next_cleanup().unwrap().unwrap();
    assert_eq!(close.backing, 9);
    assert!(sources.next_cleanup().unwrap().is_none());
    assert!(a.pending(key), "queuing/beginning close is not acknowledgement");
    sources.finish_close(close).unwrap();
    retry(&mut sources, &a, &catalog);
    assert_eq!((a.summary().live, a.summary().pending), (0, 0));
}
#[test]
fn close_ack_before_dispatch_retains_payload_until_unique_ticket_arrives()
{
    let (mut sources, _catalog) = fixture();
    let a = owner();
    let id = sources.publish(&a, FILE, &11, 20).unwrap();
    let close = sources.begin_close(&a, id).unwrap();
    let key = sources.entries.key(id).unwrap();
    let ticket = a.invalidate(key).unwrap();
    sources.finish_close(close).unwrap();
    assert!(a.pending(key));
    assert_eq!(sources.entries.retained_values().count(), 1);
    sources.release(ticket, false).unwrap();
    assert!(!a.pending(key));
    assert_eq!(sources.entries.retained_values().count(), 0);
}
#[test]
fn growable_capabilities_reserve_cleanup_returns_before_publication()
{
    let (mut sources, catalog) = fixture();
    let a = owner();
    let mut ids = Vec::new();
    for cnid in 2..322
    {
        ids.push(sources.publish(&a,
                                 Identity { cnid,
                                            ..FILE },
                                 &cnid,
                                 20)
                        .unwrap());
    }
    let capacity = sources.ready.capacity();
    drain(&mut sources, &a, &catalog);
    assert_eq!(sources.ready.capacity(), capacity);
    for expected in 2..322
    {
        let close = sources.next_cleanup().unwrap().unwrap();
        assert_eq!(close.backing, expected);
        sources.finish_close(close).unwrap();
    }
    assert!(sources.next_cleanup().unwrap().is_none());
    retry(&mut sources, &a, &catalog);
    assert_eq!(sources.ready.capacity(), capacity);
    assert_eq!(a.summary().pending, 0);
    for id in ids
    {
        assert_eq!(sources.info(&a, id), Err(NOT_FOUND));
    }
}
#[test]
fn losing_manager_keeps_received_ticket_payload_and_backing_pending()
{
    let (mut sources, _catalog) = fixture();
    let a = owner();
    let id = sources.publish(&a, FILE, &9, 20).unwrap();
    let key = sources.entries.key(id).unwrap();
    sources.release(a.invalidate(key).unwrap(), false).unwrap();
    drop(sources);
    assert!(a.pending(key));
    assert_eq!(a.summary().pending, 1);
}

#[test]
fn unsent_close_refusal_restores_the_prior_state_and_real_failure_allows_close_only_retry()
{
    let (mut sources, _catalog) = fixture();
    let a = owner();
    let id = sources.publish(&a, FILE, &11, 20).unwrap();
    let close = sources.begin_close(&a, id).unwrap();
    sources.close_unsubmitted(close).unwrap();
    assert_eq!(sources.info(&a, id), Ok(Info { length: 20 }));
    let read = sources.begin_read(&a, id, 99, 0, 1).unwrap();
    sources.finish_read(read).unwrap();
    let close = sources.begin_close(&a, id).unwrap();
    sources.close_failed(close).unwrap();
    assert_eq!(sources.info(&a, id), Err(ROLLBACK_FAILED));
    assert_eq!(sources.begin_read(&a, id, 100, 0, 1).err(), Some(ROLLBACK_FAILED));
    let close = sources.begin_close(&a, id).unwrap();
    sources.close_unsubmitted(close).unwrap();
    assert_eq!(sources.info(&a, id),
               Err(ROLLBACK_FAILED),
               "failed retry cannot reopen uncertain reads");
    let close = sources.begin_close(&a, id).unwrap();
    sources.finish_close(close).unwrap();
    assert_eq!(sources.info(&a, id), Err(NOT_FOUND));
}
