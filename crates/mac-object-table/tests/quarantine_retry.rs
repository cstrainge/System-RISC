//! Retry is one bounded metadata pass, independent of pool population balance.
use std::{alloc::{GlobalAlloc, Layout, System},
          cell::Cell};

use mac_object_table::{domain::{CapacityPolicy, CompletionError, DomainKind, KindCode,
                                OwnerIndex, ReleasedResource, ResourceKey},
                       registry::{CleanupFailure, KindLease, Registry, Release, Source,
                                  Specification},
                       shared::{InvocationGate, OwnerRef},
                       store::Store,
                       walk};

thread_local! { static ARMED: Cell<bool> = const { Cell::new(false) }; static ALLOCATIONS: Cell<usize> = const { Cell::new(0) }; }
struct Allocator;
unsafe impl GlobalAlloc for Allocator
{
    unsafe fn alloc(&self, layout: Layout) -> *mut u8
    {
        if ARMED.with(Cell::get)
        {
            ALLOCATIONS.with(|count| count.set(count.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8
    {
        if ARMED.with(Cell::get)
        {
            ALLOCATIONS.with(|count| count.set(count.get() + 1));
        }
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8
    {
        if ARMED.with(Cell::get)
        {
            ALLOCATIONS.with(|count| count.set(count.get() + 1));
        }
        unsafe { System.realloc(pointer, layout, size) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout)
    {
        unsafe { System.dealloc(pointer, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;
struct Observe;
impl Observe
{
    fn begin() -> Self
    {
        ALLOCATIONS.with(|count| count.set(0));
        ARMED.with(|armed| {
                 assert!(!armed.replace(true));
             });
        Self
    }

    fn finish(self) -> usize
    {
        ARMED.with(|armed| armed.set(false));
        ALLOCATIONS.with(Cell::get)
    }
}
impl Drop for Observe
{
    fn drop(&mut self)
    {
        ARMED.with(|armed| armed.set(false));
    }
}

struct Gate;
impl InvocationGate for Gate
{
    fn run<R>(operation: impl FnOnce() -> R) -> R
    {
        operation()
    }
}
type Owner = OwnerRef<Gate>;
fn owner() -> Owner
{
    Owner::try_new(OwnerIndex::issue(DomainKind::Module).unwrap()).unwrap()
}
struct Context
{
    owner:     Owner,
    fail:      bool,
    seen:      Vec<ResourceKey>,
    stores:    Vec<Store<(), Gate>>,
    inject:    Option<Vec<ResourceKey>>,
    add_pool:  Option<Store<(), Gate>>,
    grow_pool: Option<KindCode>
}
struct Cleanup;
impl Release<Context> for Cleanup
{
    fn release(&self, context: &mut Context, ticket: ReleasedResource)
               -> Result<(), CompletionError>
    {
        context.seen.push(ticket.key());
        if let Some(keys) = context.inject.take()
        {
            for key in keys
            {
                context.owner.quarantine(context.owner.invalidate(key).unwrap()).unwrap();
            }
        }
        if let Some(mut store) = context.add_pool.take()
        {
            let id = store.insert(&context.owner, ()).unwrap();
            let key = store.key(id).unwrap();
            context.owner.quarantine(context.owner.invalidate(key).unwrap()).unwrap();
            context.stores.push(store);
        }
        if let Some(kind) = context.grow_pool.take()
        {
            let store = context.stores.iter_mut().find(|store| store.kind() == kind).unwrap();
            let id = store.insert(&context.owner, ()).unwrap();
            let key = store.key(id).unwrap();
            context.owner.quarantine(context.owner.invalidate(key).unwrap()).unwrap();
        }
        if context.fail
        {
            Err(CompletionError::registration_mismatch(ticket))
        }
        else
        {
            let kind = ticket.key().kind();
            let store = context.stores.iter_mut().find(|store| store.kind() == kind).unwrap();
            store.release(ticket).map(drop)
        }
    }
}
fn register(registry: &mut Registry<Context, Gate>, registrar: &Owner, kind: KindCode)
            -> KindLease<Gate>
{
    registry.register(registrar,
                      Specification { kind,
                                      label: "Retry fixture",
                                      provider: "Retry fixture",
                                      capacity: CapacityPolicy::GrowableUntilMemory,
                                      source: Source::Resident },
                      Cleanup)
            .unwrap()
}
fn pool(owner: &Owner, kind: KindLease<Gate>, slots: usize) -> (Store<(), Gate>, Vec<ResourceKey>)
{
    let mut store = Store::registered(kind);
    let keys = (0..slots).map(|_| {
                             let id = store.insert(owner, ()).unwrap();
                             store.key(id).unwrap()
                         })
                         .collect();
    (store, keys)
}
fn quarantine(owner: &Owner, key: ResourceKey)
{
    owner.quarantine(owner.invalidate(key).unwrap()).unwrap();
}

#[test]
fn uneven_multi_pool_retry_attempts_every_original_ticket_once_without_allocating()
{
    let registrar = owner();
    let owner = owner();
    let mut registry = Registry::new();
    let kinds = [KindCode(*b"ONE1"), KindCode(*b"MANY"), KindCode(*b"ONE2"), KindCode(*b"MORE")];
    let mut keys = Vec::new();
    let mut stores = Vec::new();
    for (kind, count) in kinds.into_iter().zip([1, 65, 1, 3])
    {
        let kind = register(&mut registry, &registrar, kind);
        let (store, new_keys) = pool(&owner, kind, count);
        stores.push(store);
        keys.extend(new_keys);
    }
    for key in &keys
    {
        quarantine(&owner, *key);
    }
    owner.stop_admission();
    let mut context = Context { owner: owner.clone(),
                                fail: true,
                                seen: Vec::with_capacity(keys.len()),
                                stores,
                                inject: None,
                                add_pool: None,
                                grow_pool: None };
    for _ in 0..2
    {
        let observed = Observe::begin();
        let report = walk::retry(&mut context,
                                 &owner,
                                 |_, key| registry.dispatch(key.kind()),
                                 |_, _| {}).unwrap();
        let allocations = observed.finish();
        assert_eq!(allocations, 0, "retry allocated after all metadata admission");
        assert_eq!(report.quarantined, keys.len());
        assert_eq!(report.invalidated, 0);
        assert_eq!(context.seen, keys, "unequal pool sizes repeated or skipped an exact key");
        assert_eq!(owner.summary().quarantined, keys.len());
        context.seen.clear();
    }
    context.fail = false;
    let observed = Observe::begin();
    let report = walk::retry(&mut context,
                             &owner,
                             |_, key| registry.dispatch(key.kind()),
                             |_, error| panic!("retry failed: {error}")).unwrap();
    let allocations = observed.finish();
    assert_eq!(allocations, 0);
    assert_eq!(report.released, keys.len());
    assert_eq!(context.seen, keys);
    assert_eq!((owner.summary().pending, owner.summary().quarantined), (0, 0));
}

#[test]
fn new_quarantine_ahead_joins_but_passed_slots_wait_for_the_next_retry()
{
    let registrar = owner();
    let owner = owner();
    let kind = KindCode(*b"NEWQ");
    let mut registry = Registry::new();
    let kind = register(&mut registry, &registrar, kind);
    let (store, keys) = pool(&owner, kind, 3);
    quarantine(&owner, keys[1]);
    let mut context = Context { owner:     owner.clone(),
                                fail:      true,
                                seen:      Vec::with_capacity(3),
                                stores:    vec![store],
                                inject:    Some(vec![keys[0], keys[2]]),
                                add_pool:  None,
                                grow_pool: None };
    let observed = Observe::begin();
    let report = walk::retry(&mut context,
                             &owner,
                             |_, key| registry.dispatch(key.kind()),
                             |_, _| {}).unwrap();
    let allocations = observed.finish();
    assert_eq!(allocations, 0);
    assert_eq!(context.seen, [keys[1], keys[2]]);
    assert_eq!(report.quarantined, 2);
    assert_eq!(owner.summary().quarantined, 3);
    context.seen.clear();
    context.fail = false;
    walk::retry(&mut context,
                &owner,
                |_, key| registry.dispatch(key.kind()),
                |_, error| panic!("retry failed: {error}")).unwrap();
    assert_eq!(context.seen, keys);
    assert_eq!(owner.summary().pending, 0);
}

#[test]
fn pools_added_by_a_callback_are_deferred_to_the_next_pass()
{
    let registrar = owner();
    let owner = owner();
    let first = KindCode(*b"OLDP");
    let later = KindCode(*b"NEWP");
    let mut registry = Registry::new();
    let first = register(&mut registry, &registrar, first);
    let later = register(&mut registry, &registrar, later);
    let (store, keys) = pool(&owner, first, 1);
    quarantine(&owner, keys[0]);
    let mut context = Context { owner:     owner.clone(),
                                fail:      true,
                                seen:      Vec::with_capacity(2),
                                stores:    vec![store],
                                inject:    None,
                                add_pool:  Some(Store::registered(later)),
                                grow_pool: None };
    let report = walk::retry(&mut context,
                             &owner,
                             |_, key| registry.dispatch(key.kind()),
                             |_, _| {}).unwrap();
    assert_eq!(context.seen, keys);
    assert_eq!(report.quarantined, 1);
    assert_eq!(owner.summary().quarantined, 2);
    context.seen.clear();
    context.fail = false;
    walk::retry(&mut context,
                &owner,
                |_, key| registry.dispatch(key.kind()),
                |_, error| panic!("retry failed: {error}")).unwrap();
    assert_eq!(context.seen.len(), 2);
    assert_eq!(context.seen[0], keys[0]);
    assert_eq!(context.seen[1].kind(), KindCode(*b"NEWP"));
    assert_eq!(owner.summary().pending, 0);
}

#[test]
fn growth_beyond_a_visited_pool_bound_waits_for_the_next_pass()
{
    let registrar = owner();
    let owner = owner();
    let kind = KindCode(*b"GROW");
    let mut registry = Registry::new();
    let registration = register(&mut registry, &registrar, kind);
    let (store, keys) = pool(&owner, registration, 1);
    quarantine(&owner, keys[0]);
    let mut context = Context { owner:     owner.clone(),
                                fail:      true,
                                seen:      Vec::with_capacity(2),
                                stores:    vec![store],
                                inject:    None,
                                add_pool:  None,
                                grow_pool: Some(kind) };
    let report = walk::retry(&mut context,
                             &owner,
                             |_, key| registry.dispatch(key.kind()),
                             |_, _| {}).unwrap();
    assert_eq!(context.seen, keys);
    assert_eq!(report.quarantined, 1);
    assert_eq!(owner.summary().quarantined, 2);
    context.seen.clear();
    context.fail = false;
    walk::retry(&mut context,
                &owner,
                |_, key| registry.dispatch(key.kind()),
                |_, error| panic!("retry failed: {error}")).unwrap();
    assert_eq!(context.seen.len(), 2);
    assert_eq!(context.seen[0], keys[0]);
    assert_eq!(context.seen[1].slot(), 1);
    assert_eq!(owner.summary().pending, 0);
}
