extern crate std;
use mac_object_table::{domain::{DomainKind, OwnerIndex},
                       registry::Release};

use super::*;
use crate::toolbox::handles::{OwnerRef, Registry};

fn owner() -> OwnerRef
{
    OwnerRef::try_new(OwnerIndex::issue(DomainKind::Process).unwrap()).unwrap()
}
struct Ignore;
impl Release<(), Failure> for Ignore
{
    fn release(&self, _: &mut (), ticket: ReleasedResource) -> Result<(), Failure>
    {
        Err(Failure::context_unavailable(ticket, "test must dispatch its actual operation store"))
    }
}
fn operations() -> (Registry<()>, Arc<Operations>)
{
    let registrar = owner();
    let mut registry = Registry::new();
    let payload = registry.register(&registrar, specification(false), Ignore).unwrap();
    let backing = registry.register(&registrar, specification(true), Ignore).unwrap();
    let operations = Operations::new((payload, backing)).unwrap();
    (registry, operations)
}

#[test]
fn stopped_subject_keeps_exact_root_until_last_owned_use_finishes()
{
    let (_registry, operations) = operations();
    let owner = owner();
    let subject = operations.admit(&Authority::Client(owner.clone())).unwrap();
    let scope = subject.scope().clone();
    let root = scope.root().unwrap();
    let callback = subject.clone();
    owner.stop_admission();
    let ticket = owner.invalidate(root).unwrap();
    operations.release(ticket, false).unwrap();
    assert!(!owner.contains(root));
    assert!(owner.pending(root));
    scope.admitting().unwrap();
    assert_eq!(scope.subject(), owner.owner());
    assert!(matches!(operations.admit(&Authority::Client(owner.clone())),
                     Err(mac_abi::SERVICE_BUSY)));
    drop(subject);
    assert!(owner.pending(root), "detached callback is still a real use");
    drop(callback);
    assert!(!owner.pending(root));
    let error = scope.admitting().unwrap_err();
    assert!(matches!(error, mac_object_table::scope::Failure::Closed));
    assert!(alloc::format!("{error}").contains("resource scope is closed"));
}

#[test]
fn natural_completion_invalidates_root_without_waiting_for_an_owner_walk()
{
    let (_registry, operations) = operations();
    let owner = owner();
    let first = operations.admit(&Authority::Client(owner.clone())).unwrap();
    let stale = first.scope().clone();
    let root = stale.root().unwrap();
    drop(first);
    assert!(!owner.contains(root));
    assert!(!owner.pending(root));
    let next = operations.admit(&Authority::Client(owner.clone())).unwrap();
    assert_ne!(next.scope().root(), Some(root));
    assert!(stale.admitting().is_err());
    assert!(matches!(operations.admit(&Authority::Operation(stale)), Err(mac_abi::SERVICE_BUSY)));
    drop(next);
    assert_eq!(owner.summary().pending, 0);
}

#[test]
fn dispatcher_can_arrive_after_last_use_without_fabricating_an_acknowledgement()
{
    let (_registry, operations) = operations();
    let owner = owner();
    let subject = operations.admit(&Authority::Client(owner.clone())).unwrap();
    let root = subject.scope().root().unwrap();
    let ticket = owner.invalidate(root).unwrap();
    drop(subject);
    assert!(owner.pending(root), "only the unique outstanding dispatcher owns completion");
    operations.release(ticket, false).unwrap();
    assert!(!owner.pending(root));
}

#[test]
fn inherited_scope_stays_with_external_root_and_never_reopens_general_admission()
{
    let (_registry, operations) = operations();
    let owner = owner();
    let external = operations.admit(&Authority::Client(owner.clone())).unwrap();
    let root = external.scope().root().unwrap();
    owner.stop_admission();
    let inherited = operations.admit(&Authority::Operation(external.scope().clone())).unwrap();
    assert!(inherited.scope().same_scope(external.scope()));
    drop(inherited);
    assert!(owner.contains(root));
    assert!(!owner.accepting());
    drop(external);
    assert!(!owner.contains(root));
}

#[test]
fn final_owned_use_and_cleanup_dispatch_can_race_without_leaking_or_double_acknowledging()
{
    use alloc::sync::Arc;
    use std::sync::Barrier;
    let (_registry, operations) = operations();
    for _ in 0..64
    {
        let owner = owner();
        let subject = operations.admit(&Authority::Client(owner.clone())).unwrap();
        let scope = subject.scope().clone();
        let root = scope.root().unwrap();
        owner.stop_admission();
        let ticket = owner.invalidate(root).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let other = barrier.clone();
        let dispatcher = operations.clone();
        let thread = std::thread::spawn(move || {
            other.wait();
            dispatcher.release(ticket, false).unwrap();
        });
        barrier.wait();
        drop(subject);
        thread.join().unwrap();
        assert!(!owner.pending(root));
        assert!(matches!(scope.admitting(), Err(mac_object_table::scope::Failure::Closed)));
        let backing = owner.invalidate_next().unwrap();
        assert_eq!(backing.key().kind(), KindCode(*b"DIOB"));
        operations.release(backing, true).unwrap();
        assert_eq!((owner.summary().live, owner.summary().pending), (0, 0));
    }
}
