use mac_object_table::{domain::{CapacityPolicy, DomainKind, KindCode, OwnerIndex, PoolError},
                       scope::{Failure, Reservation},
                       shared::{InvocationGate, OwnerRef},
                       store::Store};
struct Gate;
impl InvocationGate for Gate
{
    fn run<R>(f: impl FnOnce() -> R) -> R
    {
        f()
    }
}
const ROOT: KindCode = KindCode(*b"ROOT");
const CHILD: KindCode = KindCode(*b"CHLD");
fn owner() -> OwnerRef<Gate>
{
    OwnerRef::try_new(OwnerIndex::issue(DomainKind::Process).unwrap()).unwrap()
}

#[test]
fn scope_admits_existing_cleanup_after_stop_without_reopening_owner_admission()
{
    let owner = owner();
    let mut roots = Store::new(ROOT, CapacityPolicy::PerDomainQuota { slots: 1 });
    let mut children = Store::new(CHILD, CapacityPolicy::PerDomainQuota { slots: 1 });
    let reservation = Reservation::try_new(&owner, ROOT).unwrap();
    let lease = reservation.lease();
    assert_eq!(lease.admitting(), Err(Failure::Unpublished));
    let error = children.insert_in_scope(&lease, 1).unwrap_err();
    assert!(matches!(error,
                     PoolError::Scope { cause: Failure::Unpublished,
                                        .. }));
    assert!(error.to_string().contains("root resource is not published"));
    assert_eq!(owner.summary().live, 0);
    let root_id = roots.insert_root(reservation, 42).unwrap();
    let root = lease.root().unwrap();
    assert_eq!((root.owner(), root.kind()), (owner.owner(), ROOT));
    lease.admitting().unwrap();
    let first = children.insert_in_scope(&lease, 10).unwrap();
    assert_eq!(children.owner(first), Some(owner.owner()));
    assert_eq!(owner.summary().scoped_children, 1);
    let root_guard = roots.remove(root_id).unwrap();
    owner.stop_admission();
    assert!(!owner.accepting());
    assert!(!lease.subject_accepting());
    lease.admitting().unwrap(); // The root's actual pending guard owns continuation.
    let ticket = owner.invalidate_next().unwrap();
    let child = ticket.key();
    assert_eq!(child.kind(), CHILD);
    assert_eq!(owner.parent(child), Some(root));
    let pending = children.release(ticket).unwrap();
    assert!(children.get(first).is_none());
    let error = children.insert_in_scope(&lease, 11).unwrap_err();
    assert!(matches!(error,
                     PoolError::Exhausted { pending: 1,
                                            .. }));
    assert!(error.to_string().contains("1 pending cleanup"));
    drop(pending);
    assert_eq!(owner.parent(child), None);
    let second = children.insert_in_scope(&lease, 12).unwrap();
    assert_ne!(second, first);
    assert!(children.get(first).is_none());
    let error = children.insert(&owner, 13).unwrap_err();
    assert!(matches!(error, PoolError::Stopping { .. }));
    assert!(error.to_string().contains("new handles are refused"));
    assert!(!owner.accepting(), "scoped admission must not reopen general admission");
    drop(children.remove(second));
    drop(root_guard); // The store closes authority after real payload destruction.
    assert_eq!(lease.admitting(), Err(Failure::Closed));
    let error = children.insert_in_scope(&lease, 14).unwrap_err();
    assert!(matches!(error,
                     PoolError::Scope { cause: Failure::Closed,
                                        .. }));
    assert!(error.to_string().contains("resource scope is closed"));
    assert_eq!((owner.summary().live, owner.summary().pending, owner.summary().scoped_children),
               (0, 0, 0));
}

#[test]
fn root_scope_is_bound_to_its_kind_and_generation_and_cannot_alias_slot_reuse()
{
    let owner = owner();
    let mut roots = Store::new(ROOT, CapacityPolicy::PerDomainQuota { slots: 1 });
    let mut other = Store::new(CHILD, CapacityPolicy::PerDomainQuota { slots: 1 });
    let wrong = Reservation::try_new(&owner, ROOT).unwrap();
    let stale_unpublished = wrong.lease();
    let error = other.insert_root(wrong, 0).unwrap_err();
    assert!(matches!(error,
                     PoolError::Scope { cause: Failure::WrongKind { expected: ROOT,
                                                                    actual:   CHILD },
                                        .. }));
    assert!(error.to_string().contains("publish through the matching root store"));
    assert_eq!(stale_unpublished.admitting(), Err(Failure::Closed));
    assert_eq!(owner.summary().live, 0);
    let first = Reservation::try_new(&owner, ROOT).unwrap();
    let old_scope = first.lease();
    let old_id = roots.insert_root(first, 1).unwrap();
    let old = old_scope.root().unwrap();
    drop(roots.remove(old_id));
    let second = Reservation::try_new(&owner, ROOT).unwrap();
    let current = second.lease();
    let new_id = roots.insert_root(second, 2).unwrap();
    let fresh = current.root().unwrap();
    assert_ne!(old_id, new_id);
    assert_eq!(old.slot(), fresh.slot());
    assert_ne!(old.generation(), fresh.generation());
    assert!(!old_scope.same_scope(&current));
    assert_eq!(old_scope.admitting(), Err(Failure::Closed));
    current.admitting().unwrap();
    assert!(roots.get(old_id).is_none());
}

#[test]
fn root_destructor_can_admit_a_child_before_scope_closure_but_never_after()
{
    use std::sync::{Arc, Mutex,
                    atomic::{AtomicU64, Ordering}};
    struct Root
    {
        scope:    mac_object_table::scope::Lease<Gate>,
        children: Arc<Mutex<Store<u32, Gate>>>,
        child:    Arc<AtomicU64>
    }
    impl Drop for Root
    {
        fn drop(&mut self)
        {
            let child = self.children.lock().unwrap().insert_in_scope(&self.scope, 123).unwrap();
            self.child.store(child, Ordering::Relaxed);
        }
    }
    let owner = owner();
    let mut roots = Store::new(ROOT, CapacityPolicy::GrowableUntilMemory);
    let children = Arc::new(Mutex::new(Store::new(CHILD, CapacityPolicy::GrowableUntilMemory)));
    let child = Arc::new(AtomicU64::new(0));
    let reservation = Reservation::try_new(&owner, ROOT).unwrap();
    let scope = reservation.lease();
    let id = roots.insert_root(reservation,
                               Root { scope:    scope.clone(),
                                      children: children.clone(),
                                      child:    child.clone() })
                  .unwrap();
    owner.stop_admission();
    drop(roots.remove(id));
    let child_id = child.load(Ordering::Relaxed);
    assert_ne!(child_id, 0);
    assert_eq!(children.lock().unwrap().get(child_id), Some(&123));
    assert_eq!(scope.admitting(), Err(Failure::Closed));
    assert_eq!((owner.summary().live, owner.summary().pending, owner.summary().scoped_children),
               (1, 0, 1));
    drop(children.lock().unwrap().remove(child_id));
    assert_eq!((owner.summary().live, owner.summary().pending, owner.summary().scoped_children),
               (0, 0, 0));
}
