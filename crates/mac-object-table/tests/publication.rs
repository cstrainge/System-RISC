//! Controlled handoff at the real invocation gate, after owner metadata unlock.
//! This is host interleaving evidence, not a native multihart/recovery claim.
use std::{cell::RefCell, rc::Rc};

use mac_object_table::{domain::{CapacityPolicy, DomainKind, KindCode, OwnerIndex,
                                ReleasedResource},
                       shared::{InvocationGate, OwnerRef},
                       store::Store};
thread_local! {
    static AFTER:RefCell<Option<Box<dyn FnMut()->bool>>>=RefCell::new(None);
}
struct Gate;
impl InvocationGate for Gate
{
    fn run<R>(f: impl FnOnce() -> R) -> R
    {
        let result = f();
        let hook = AFTER.with(|slot| slot.borrow_mut().take());
        if let Some(mut hook) = hook
        {
            if !hook()
            {
                AFTER.with(|slot| *slot.borrow_mut() = Some(hook));
            }
        }
        result
    }
}
const KIND: KindCode = KindCode(*b"RACE");
fn owner() -> OwnerRef<Gate>
{
    OwnerRef::try_new(OwnerIndex::issue(DomainKind::Process).unwrap()).unwrap()
}
fn invalidate_after_publication(owner: &OwnerRef<Gate>, baseline: usize)
                                -> Rc<RefCell<Option<ReleasedResource>>>
{
    let ticket = Rc::new(RefCell::new(None));
    let captured = ticket.clone();
    let owner = owner.clone();
    AFTER.with(|slot| {
             assert!(slot.borrow().is_none());
             *slot.borrow_mut() = Some(Box::new(move || {
                                           if owner.summary().live <= baseline
                                           {
                                               return false;
                                           }
                                           owner.stop_admission();
                                           let issued = owner.invalidate_next().unwrap();
                                           assert_eq!(issued.key().kind(), KIND);
                                           *captured.borrow_mut() = Some(issued);
                                           true
                                       }));
         });
    ticket
}
#[test]
fn ordinary_ids_cannot_recreate_completion_authority_after_publication_was_invalidated()
{
    let owner = owner();
    let mut store = Store::new(KIND, CapacityPolicy::GrowableUntilMemory);
    let captured = invalidate_after_publication(&owner, 0);
    let id = store.insert(&owner, "retained").unwrap();
    assert!(store.get(id).is_none());
    assert!(store.completion_key(id).is_none(),
            "ordinary stale lookup must not mint new completion authority");
    let ticket = captured.borrow_mut().take().unwrap();
    assert_eq!(store.releasing(&ticket), Some(&"retained"));
    drop(store.release(ticket).unwrap());
    assert_eq!((owner.summary().live, owner.summary().pending), (0, 0));
    AFTER.with(|slot| assert!(slot.borrow().is_none()));
}

#[test]
fn native_publication_keeps_exact_completion_provenance_through_the_handoff()
{
    let owner = owner();
    let mut store = Store::new(KIND, CapacityPolicy::GrowableUntilMemory);
    let captured = invalidate_after_publication(&owner, 0);
    let publication = store.insert_with_receipt(&owner, |id| (id, "retained")).unwrap();
    let (id, completion) = publication.into_parts();
    assert!(id > u64::from(u32::MAX));
    assert!(store.get(id).is_none());
    assert!(store.completion_key(id).is_none());
    assert_eq!(store.completing(&completion), Some(&(id, "retained")));
    let other = OwnerRef::<Gate>::try_new(OwnerIndex::issue(DomainKind::Process).unwrap()).unwrap();
    let mut foreign = Store::new(KIND, CapacityPolicy::GrowableUntilMemory);
    let other_id = foreign.insert(&other, (0, "foreign")).unwrap();
    assert!(foreign.completing(&completion).is_none());
    let ticket = captured.borrow_mut().take().unwrap();
    drop(store.release(ticket).unwrap());
    assert!(store.completing(&completion).is_none(),
            "receipt metadata must not pin or revive completed payloads");
    drop(foreign.remove(other_id));
    assert_eq!((owner.summary().live, owner.summary().pending), (0, 0));
}

use mac_object_table::{Kind,
                       domain::CompletionError,
                       projected,
                       registry::{Registry, Release, Source, Specification},
                       scope::Reservation};
struct Retain;
impl Release<()> for Retain
{
    fn release(&self, _: &mut (), ticket: ReleasedResource) -> Result<(), CompletionError>
    {
        Err(<CompletionError as mac_object_table::registry::CleanupFailure>::registration_mismatch(ticket))
    }
}
fn projected_store() -> (Registry<(), Gate>, projected::Store<&'static str, Gate>)
{
    let registrar = OwnerRef::try_new(OwnerIndex::issue(DomainKind::Module).unwrap()).unwrap();
    let mut registry = Registry::new();
    let mut kind = |code| {
        registry.register(&registrar,
                          Specification { kind:     KindCode(code),
                                          label:    "Publication fixture",
                                          provider: "Host gate",
                                          capacity: CapacityPolicy::GrowableUntilMemory,
                                          source:   Source::Resident },
                          Retain)
                .unwrap()
    };
    let payload = kind(KIND.0);
    let backing = kind(*b"BACK");
    (registry, projected::Store::new(Kind::Files, payload, backing).unwrap())
}
#[test]
fn treaty_publication_keeps_its_wire_and_native_completion_before_the_owner_walk()
{
    for scoped_root in [false, true]
    {
        let owner = owner();
        let (_registry, mut store) = projected_store();
        let reservation = scoped_root.then(|| Reservation::try_new(&owner, KIND).unwrap());
        // Real registered backing is installed before the semantic payload.
        let captured = invalidate_after_publication(&owner, 1);
        let publication = if let Some(reservation) = reservation
        {
            store.insert_root_with_receipt(reservation, |_| "retained").unwrap()
        }
        else
        {
            store.insert_with_receipt(&owner, "retained").unwrap()
        };
        let (wire, completion) = publication.into_parts();
        assert!(Kind::Files.contains_wire_id(wire));
        assert!(store.get(wire).is_none());
        assert!(store.completion_key(wire).is_none());
        assert_eq!(store.completing(&completion), Some(&"retained"));
        let ticket = captured.borrow_mut().take().unwrap();
        let guard = store.release(ticket).unwrap();
        assert_eq!(*guard, "retained");
        drop(guard);
        assert!(store.completing(&completion).is_none());
        let backing = owner.invalidate_next().unwrap();
        assert_eq!(backing.key().kind(), KindCode(*b"BACK"));
        store.release_storage(backing).unwrap();
        assert_eq!((owner.summary().live, owner.summary().pending), (0, 0));
    }
}

#[test]
fn root_and_child_receipts_preserve_existing_cleanup_without_reopening_admission()
{
    let owner = owner();
    let mut roots = Store::new(KIND, CapacityPolicy::GrowableUntilMemory);
    let mut children = Store::new(KindCode(*b"CHLD"), CapacityPolicy::GrowableUntilMemory);
    let root = Reservation::try_new(&owner, KIND).unwrap();
    let lease = root.lease();
    let captured = invalidate_after_publication(&owner, 0);
    let (id, key) = roots.insert_root_with_receipt(root, |_| "root").unwrap().into_parts();
    assert!(roots.get(id).is_none());
    assert_eq!(roots.completing(&key), Some(&"root"));
    assert!(!owner.accepting());
    lease.admitting().unwrap();
    let (child, completion) =
        children.insert_in_scope_with_receipt(&lease, |id| id).unwrap().into_parts();
    let ticket = owner.invalidate_next().unwrap();
    assert_eq!(ticket.key().kind(), KindCode(*b"CHLD"));
    assert_eq!(owner.parent(ticket.key()), lease.root());
    assert!(children.get(child).is_none());
    assert_eq!(children.completing(&completion), Some(&child));
    drop(children.release(ticket).unwrap());
    assert!(children.completing(&completion).is_none());
    let ticket = captured.borrow_mut().take().unwrap();
    drop(roots.release(ticket).unwrap());
    assert!(roots.completing(&key).is_none());
    assert!(matches!(lease.admitting(), Err(mac_object_table::scope::Failure::Closed)));
    assert!(!owner.accepting());
    assert_eq!((owner.summary().live, owner.summary().pending), (0, 0));
}

#[test]
fn receipt_admission_refuses_pending_quota_and_stopped_owners_with_the_real_cause()
{
    let owner = owner();
    let mut store = Store::new(KIND, CapacityPolicy::PerDomainQuota { slots: 1 });
    let (id, key) = store.insert_with_receipt(&owner, |_| 1).unwrap().into_parts();
    let guard = store.remove(id).unwrap();
    let error = store.insert_with_receipt(&owner, |_| 2).err().unwrap();
    assert!(matches!(error,
                     mac_object_table::domain::PoolError::Exhausted { pending: 1,
                                                                      .. }));
    assert!(error.to_string().contains("1 pending cleanup"));
    assert!(store.completing(&key).is_none(), "linear removal owns this payload now");
    assert_eq!(*guard, 1);
    drop(guard);
    let fresh = store.insert_with_receipt(&owner, |_| 3).unwrap();
    assert_ne!(fresh.id(), id);
    assert!(store.completing(&key).is_none());
    owner.stop_admission();
    let error = store.insert_with_receipt(&owner, |_| panic!("stopped admission ran the factory"))
                     .err()
                     .unwrap();
    assert!(matches!(error, mac_object_table::domain::PoolError::Stopping { .. }));
    assert!(error.to_string().contains("new handles are refused"));
    drop(store.remove(fresh.id()));
    assert_eq!((owner.summary().live, owner.summary().pending), (0, 0));
}
