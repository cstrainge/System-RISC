//! Owner-ledger-backed values. Only this module may publish or complete cleanup
//! in an attached pool; object payloads cannot be swapped behind a live key.
//! No destructor or callback runs inside the owner metadata lock.
use alloc::{sync::Arc, vec::Vec};
use core::{mem::ManuallyDrop, ops::Deref};

use hashbrown::HashMap;

use crate::{domain::{CapacityPolicy, CompletionError, DomainId, KindCode, PoolError,
                     ReleasedResource, ResourceKey, issue_native_identity},
            registry::{KindLease, Use},
            shared::{InvocationGate, OwnerRef}};

struct Claim<G: InvocationGate>
{
    owner:  OwnerRef<G>,
    kind:   KindCode,
    active: bool
}
impl<G: InvocationGate> Drop for Claim<G>
{
    fn drop(&mut self)
    {
        if self.active
        {
            self.owner.access(|index| index.detach_values(self.kind));
        }
    }
}
struct Entry<T>
{
    id:               u64,
    key:              ResourceKey,
    value:            T,
    registration_use: ManuallyDrop<Option<Use>>
}
struct Values<T, G: InvocationGate>
{
    slots:  Vec<Option<Entry<T>>>,
    claim:  Arc<Claim<G>>,
    stored: usize
}

/// A registered semantic kind's storage. Identity is issued by the shared
/// boot-wide issuer, never by an individual manager. An ID resolves to its full
/// owner/kind/slot/generation key and is validated on every use. Native tokens
/// are outside the 32-bit classic projection space; their bits are opaque.
///
/// Growth is fallible and precedes publication. Prepared backing starts at one
/// slot and doubles on demand; this is not a live-object quota. Per-owner history lives in the
/// authoritative ledger even when an empty store is dropped and recreated.
pub struct Store<T, G: InvocationGate>
{
    kind:         KindCode,
    policy:       CapacityPolicy,
    owners:       Vec<Values<T, G>>,
    ids:          HashMap<u64, ResourceKey>,
    registration: Option<KindLease<G>>
}
impl<T, G: InvocationGate> Store<T, G>
{
    pub fn new(kind: KindCode, policy: CapacityPolicy) -> Self
    {
        Self { kind,
               policy,
               owners: Vec::new(),
               ids: HashMap::new(),
               registration: None }
    }

    /// Resident stores bind policy and release identity to the registry's
    /// checked capability. The unregistered constructor is the bootstrap
    /// metadata primitive, also exercised directly by facility model tests.
    pub fn registered(registration: KindLease<G>) -> Self
    {
        let descriptor = registration.registration();
        let mut store = Self::new(descriptor.code, descriptor.capacity);
        store.registration = Some(registration);
        store
    }

    fn owner_index(&self, owner: DomainId) -> Result<usize, usize>
    {
        self.owners
            .binary_search_by_key(&owner.incarnation(), |v| v.claim.owner.owner().incarnation())
    }

    fn storage_error(&self, owner: DomainId, requested_slots: usize) -> PoolError
    {
        PoolError::Storage { owner, kind: self.kind,
            used: self.owner_index(owner).ok().map_or(0, |at| self.owners[at].claim.owner.access(|index| index.pools().find(|p| p.kind() == self.kind).map_or(0, |pool| pool.live()))),
            requested_slots, limit: self.policy.limit() }
    }

    fn attach(&mut self, owner: &OwnerRef<G>) -> Result<usize, PoolError>
    {
        let at = match self.owner_index(owner.owner())
        {
            Ok(at) => return Ok(at),
            Err(at) => at
        };
        let error = self.storage_error(owner.owner(), 1);
        self.owners.try_reserve(1).map_err(|_| error)?;
        let mut claim = Arc::try_new(Claim { owner:  owner.clone(),
                                             kind:   self.kind,
                                             active: false }).map_err(|_| error)?;
        let registration = self.registration.as_ref().map(KindLease::id);
        let slots =
            owner.access(|index| index.attach_values(self.kind, self.policy, registration))?;
        Arc::get_mut(&mut claim).unwrap().active = true;
        let mut values = Vec::new();
        values.try_reserve(slots).map_err(|_| error)?;
        values.resize_with(slots, || None);
        self.owners.insert(at,
                           Values { claim,
                                    slots: values,
                                    stored: 0 });
        Ok(at)
    }

    pub fn insert(&mut self, owner: &OwnerRef<G>, value: T) -> Result<u64, PoolError>
    {
        let mut registration_use =
            self.registration.as_ref().map(|kind| kind.acquire(true)).transpose()?;
        let at = self.attach(owner)?;
        let error = self.storage_error(owner.owner(), self.ids.len().saturating_add(1));
        self.ids.try_reserve(1).map_err(|_| error)?;
        let id = issue_native_identity().map_err(|_| PoolError::IdentityExhausted)?.get();
        let mut value = Some(value);
        loop
        {
            // Publication is atomic with respect to stop/invalidate/the walk.
            // All backing capacity was reserved before this metadata lock.
            let result = owner.access(|index| {
                             let key = index.allocate_value(self.kind)?;
                             debug_assert!(self.owners[at].slots[key.slot()].is_none());
                             self.owners[at].slots[key.slot()] =
                                 Some(Entry { id,
                                              key,
                                              value: value.take().unwrap(),
                                              registration_use:
                                                  ManuallyDrop::new(registration_use.take()) });
                             self.ids.insert(id, key);
                             self.owners[at].stored += 1;
                             Ok(())
                         });
            match result
            {
                Ok(()) => break,
                Err(PoolError::GrowthRequired { storage_slots,
                                                policy_limit,
                                                .. }) =>
                {
                    let requested =
                        storage_slots.checked_mul(2)
                                     .filter(|&n| n != 0)
                                     .unwrap_or_else(|| storage_slots.saturating_add(1));
                    let requested = policy_limit.map_or(requested, |limit| requested.min(limit));
                    let error = self.storage_error(owner.owner(), requested);
                    let slots = &mut self.owners[at].slots;
                    slots.try_reserve(requested - slots.len()).map_err(|_| error)?;
                    owner.access(|index| index.grow_values(self.kind, requested))?;
                    slots.resize_with(requested, || None);
                }
                Err(error) => return Err(error)
            }
        }
        Ok(id)
    }

    fn locate(&self, id: u64) -> Option<(usize, ResourceKey)>
    {
        let key = *self.ids.get(&id)?;
        let at = self.owner_index(key.owner()).ok()?;
        self.owners[at].claim.owner.contains(key).then_some((at, key))
    }

    pub fn get(&self, id: u64) -> Option<&T>
    {
        if self.registration.as_ref().is_some_and(|kind| !kind.live())
        {
            return None;
        }
        let (at, key) = self.locate(id)?;
        let entry = self.owners[at].slots.get(key.slot())?.as_ref()?;
        (entry.key == key && entry.id == id).then_some(&entry.value)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut T>
    {
        if self.registration.as_ref().is_some_and(|kind| !kind.live())
        {
            return None;
        }
        let (at, key) = self.locate(id)?;
        let entry = self.owners[at].slots.get_mut(key.slot())?.as_mut()?;
        (entry.key == key && entry.id == id).then_some(&mut entry.value)
    }

    pub fn owner(&self, id: u64) -> Option<DomainId>
    {
        self.locate(id).map(|(_, key)| key.owner())
    }

    pub fn key(&self, id: u64) -> Option<ResourceKey>
    {
        self.locate(id).map(|(_, key)| key)
    }

    pub fn remove(&mut self, id: u64) -> Option<Removed<T, G>>
    {
        let (at, key) = self.locate(id)?;
        let released = self.owners[at].claim.owner.invalidate(key).ok()?;
        Some(self.take(at, released))
    }

    fn take(&mut self, at: usize, released: ReleasedResource) -> Removed<T, G>
    {
        let key = released.key();
        let entry = self.owners[at].slots[key.slot()].take().expect("validated store slot");
        self.owners[at].stored -= 1;
        self.ids.remove(&entry.id);
        Removed { value:            Some(entry.value),
                  released:         Some(released),
                  claim:            self.owners[at].claim.clone(),
                  registration_use: entry.registration_use }
    }

    /// Resolve the facility walk's already-invalidated ticket. Refusal preserves
    /// it for the correct registration; only matching stored payloads can ack it.
    pub fn release(&mut self, released: ReleasedResource)
                   -> Result<Removed<T, G>, CompletionError>
    {
        let key = released.key();
        if let Ok(at) = self.owner_index(key.owner())
        {
            if key.kind() == self.kind
               && self.owners[at].slots
                                 .get(key.slot())
                                 .and_then(Option::as_ref)
                                 .is_some_and(|entry| entry.key == key)
            {
                return Ok(self.take(at, released));
            }
        }
        Err(CompletionError::stale(released))
    }

    /// Release this owner's payloads and backing value arrays. Generation history
    /// remains in its authoritative ledger until the domain itself retires.
    pub fn remove_owner(&mut self, incarnation: u64)
    {
        self.remove_where(|owner, _| owner.incarnation() == incarnation);
        self.prune_empty_owner(incarnation);
    }

    /// Backing-state cleanup during the staged manager-state migration. This
    /// never disposes values or acknowledges pending tickets. A quarantined
    /// payload keeps its backing/claim available for the registered retry.
    pub fn prune_empty_owner(&mut self, incarnation: u64) -> bool
    {
        let Ok(at) =
            self.owners.binary_search_by_key(&incarnation, |values| {
                           values.claim.owner.owner().incarnation()
                       })
        else
        {
            return true;
        };
        if self.owners[at].stored != 0
        {
            return false;
        }
        self.owners.remove(at);
        true
    }

    pub fn iter(&self) -> impl Iterator<Item = (u64, &T)>
    {
        self.owners.iter().flat_map(|values| {
                              values.slots.iter().filter_map(|slot| {
                                                     let entry = slot.as_ref()?;
                                                     values.claim
                                                           .owner
                                                           .contains(entry.key)
                                                           .then_some((entry.id, &entry.value))
                                                 })
                          })
    }

    /// No temporary ID vector and no allocation proportional to object count.
    pub fn remove_where(&mut self, mut predicate: impl FnMut(DomainId, &T) -> bool)
    {
        for at in 0..self.owners.len()
        {
            for slot in 0..self.owners[at].slots.len()
            {
                let Some(entry) = &self.owners[at].slots[slot]
                else
                {
                    continue;
                };
                if predicate(entry.key.owner(), &entry.value)
                {
                    let id = entry.id;
                    if let Some(removed) = self.remove(id)
                    {
                        drop(removed);
                    }
                }
            }
        }
    }
}
impl<T, G: InvocationGate> Drop for Store<T, G>
{
    fn drop(&mut self)
    {
        self.remove_where(|_, _| true);
        // If the walk owns a ticket, dropping its value does not acknowledge the
        // outstanding cleanup. Its pending bit remains until explicit completion.
    }
}

/// The payload dies before the slot is reusable. Forgetting a guard leaves a
/// visible pending slot; it cannot alias a replacement. A panicking destructor
/// likewise never acknowledges successful cleanup. Claim lifetime includes guards
/// that survive dropping their store, so raw metadata cannot bypass destruction.
/// A removal guard offers no mutable dereference or owned extraction: cleanup
/// cannot be acknowledged by replacing its payload and keeping the old one.
/// ```compile_fail
/// use mac_object_table::{shared::InvocationGate, store::Removed};
/// fn steal<G: InvocationGate>(mut removed: Removed<String, G>) -> String {
///     core::mem::take(&mut *removed)
/// }
/// ```
pub struct Removed<T, G: InvocationGate>
{
    value:            Option<T>,
    released:         Option<ReleasedResource>,
    claim:            Arc<Claim<G>>,
    registration_use: ManuallyDrop<Option<Use>>
}
impl<T, G: InvocationGate> Deref for Removed<T, G>
{
    type Target = T;

    fn deref(&self) -> &T
    {
        self.value.as_ref().unwrap()
    }
}
impl<T, G: InvocationGate> Drop for Removed<T, G>
{
    fn drop(&mut self)
    {
        drop(self.value.take());
        if let Some(released) = self.released.take()
        {
            self.claim
                .owner
                .access(|index| index.complete_value_release(released))
                .expect("store owns the matching cleanup capability");
        }
        // A destructor/cleanup panic leaves both the owner barrier and kind
        // pin outstanding. Unregister must not mistake failed cleanup for zero
        // users. No unsafe/manual destruction is needed to implement that rule.
        let usage = core::mem::replace(&mut self.registration_use, ManuallyDrop::new(None));
        drop(ManuallyDrop::into_inner(usage));
    }
}
