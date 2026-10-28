//! Owner-ledger-backed values. Only this module may publish or complete cleanup
//! in an attached pool; object payloads cannot be swapped behind a live key.
//! No destructor or callback runs inside the owner metadata lock.
use alloc::{sync::{Arc, Weak},
            vec::Vec};
use core::{mem::ManuallyDrop, ops::Deref};

use hashbrown::HashMap;
use spinning_top::Spinlock;

use crate::{completion::{Completion, Reserve},
            domain::{CapacityPolicy, CompletionError, DomainId, KindCode, PoolError,
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
struct Entry<T, G: InvocationGate>
{
    id:               u64,
    key:              ResourceKey,
    value:            T,
    reserve:          Reserve,
    reservation:      Option<Arc<Reserved<G>>>,
    root:             Option<crate::scope::Owner<G>>,
    registration_use: ManuallyDrop<Option<Use>>
}
struct Values<T, G: InvocationGate, S>
{
    slots:   Vec<Option<Entry<T, G>>>,
    claim:   Arc<Claim<G>>,
    stored:  usize,
    state:   Option<S>,
    storage: ManuallyDrop<Option<Storage<G>>>
}
impl<T, G: InvocationGate, S> Values<T, G, S>
{
    fn iter(&self) -> impl Iterator<Item = (u64, &T)>
    {
        self.slots.iter().filter_map(|slot| {
                             let entry = slot.as_ref()?;
                             self.claim
                                 .owner
                                 .contains(entry.key)
                                 .then_some((entry.id, &entry.value))
                         })
    }
}
impl<T, G: InvocationGate, S> Drop for Values<T, G, S>
{
    fn drop(&mut self)
    {
        let storage = core::mem::replace(&mut self.storage, ManuallyDrop::new(None));
        if let Some(mut storage) = ManuallyDrop::into_inner(storage)
        {
            storage.completion.run(|| drop(self.state.take()));
        }
        else
        {
            drop(self.state.take());
        }
    }
}

// The backing is itself a registered resource. Its release may precede its
// children: retain that unique ticket until the last payload leaves the array.
// No manager-specific finish-owner hook or teardown ordering is necessary.
struct Storage<G: InvocationGate>
{
    _claim:     Claim<G>,
    key:        ResourceKey,
    released:   bool,
    completion: Completion<G>
}
impl<G: InvocationGate> Storage<G>
{
    fn new(owner: &OwnerRef<G>, kind: &KindLease<G>, scope: Option<&crate::scope::State>)
           -> Result<Self, PoolError>
    {
        let spec = kind.registration();
        let reserve = Reserve::try_new().map_err(|_| {
                                            PoolError::Storage { owner:           owner.owner(),
                                                                 kind:            spec.code,
                                                                 used:            0,
                                                                 requested_slots: 1,
                                                                 limit:           spec.capacity
                                                                                      .limit() }
                                        })?;
        let usage = kind.acquire(true)?;
        let slots =
            owner.access(|index| {
                     index.attach_values_with(spec.code, spec.capacity, Some(kind.id()), scope)
                 })?;
        let claim = Claim { owner:  owner.clone(),
                            kind:   spec.code,
                            active: true };
        let key = owner.access(|index| {
                           if slots == 0
                           {
                               index.grow_values_with(spec.code, 1, scope)?;
                           }
                           index.allocate_value_with(spec.code, scope)
                       })?;
        Ok(Self { _claim: claim,
                  key,
                  released: false,
                  completion: reserve.live(owner.clone(), key, Some(usage)) })
    }
}
impl<G: InvocationGate> Drop for Storage<G>
{
    fn drop(&mut self)
    {
        self.completion.run(|| {});
    }
}

/// Lifetime retained through actual payload destruction and canonical cleanup
/// acknowledgement, including interrupted/refused completion quarantine. This
/// grants no object, scope, account or mutation authority. Arc coercion allows
/// an already-admitted service session to pin itself without allocating.
pub type AcknowledgementPin = Arc<dyn Send + Sync>;

/// Exclusive, preadmitted publication capacity in this exact store and owner.
/// Holding it issues no public identity and contributes no live object/birth.
/// The canonical slot, generation arrays, dedicated ID-index bucket and cleanup
/// record are unavailable to normal publishers. Cleanup refusal keeps it busy.
pub struct PublicationReservation<G: InvocationGate>
{
    inner: Arc<Reserved<G>>
}
impl<G: InvocationGate> PublicationReservation<G>
{
    pub fn ready(&self) -> bool
    {
        self.inner.completion.ready::<G>()
    }
}
struct Reserved<G: InvocationGate>
{
    claim:      Arc<Claim<G>>,
    slot:       usize,
    mapping:    Spinlock<Option<CompletionKey>>,
    completion: Arc<crate::completion::Recycling>
}
impl<G: InvocationGate> Drop for Reserved<G>
{
    fn drop(&mut self)
    {
        // Live/pending/retired occupancy still excludes reuse if a publisher
        // abandons its capability. Never reset a generation or acknowledge I/O.
        self.claim.owner.access(|index| index.unreserve_value_slot(self.claim.kind, self.slot));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicationError
{
    Busy,
    Pool(PoolError)
}
impl From<PoolError> for PublicationError
{
    fn from(error: PoolError) -> Self
    {
        Self::Pool(error)
    }
}

/// Read authority for an already-issued operation's completion. Minted only
/// from a live object; cannot be constructed from a caller's ID or ResourceKey.
/// This does not acknowledge cleanup or pin pages by itself. The retained store
/// or removal guard remains responsible for the real completion barrier.
#[derive(Clone, Copy)]
pub struct CompletionKey
{
    id:  u64,
    key: ResourceKey
}
impl CompletionKey
{
    pub fn id(self) -> u64
    {
        self.id
    }

    pub fn owner(self) -> DomainId
    {
        self.key.owner()
    }
}

/// Publication returns its authenticated completion key from the same owner
/// transaction that installs the payload. Invalidation immediately after that
/// transaction cannot require a second ordinary live lookup. This is metadata,
/// not an external-I/O ACK or an independent payload/page pin.
#[derive(Clone, Copy)]
pub struct Publication
{
    completion: CompletionKey
}
impl Publication
{
    pub fn id(self) -> u64
    {
        self.completion.id
    }

    pub fn into_parts(self) -> (u64, CompletionKey)
    {
        (self.completion.id, self.completion)
    }
}

/// A registered semantic kind's storage. Identity is issued by the shared
/// boot-wide issuer, never by an individual manager. An ID resolves to its full
/// owner/kind/slot/generation key and is validated on every use. Native tokens
/// are outside the 32-bit classic projection space; their bits are opaque.
///
/// Growth is fallible and precedes publication. Prepared backing starts at one
/// slot and doubles on demand; this is not a live-object quota. Per-owner history lives in the
/// authoritative ledger even when an empty store is dropped and recreated.
pub struct Store<T, G: InvocationGate, S = ()>
{
    kind:                 KindCode,
    policy:               CapacityPolicy,
    owners:               Vec<Values<T, G, S>>,
    ids:                  HashMap<u64, ResourceKey>,
    reserved:             Vec<Weak<Reserved<G>>>,
    registration:         Option<KindLease<G>>,
    storage_registration: Option<KindLease<G>>,
    make_state:           fn(DomainId) -> Result<S, PoolError>
}

fn find_reserved<G: InvocationGate>(reserved: &[Weak<Reserved<G>>], id: u64)
                                    -> Option<ResourceKey>
{
    G::run(|| {
        reserved.iter().filter_map(Weak::upgrade).find_map(|entry| {
                                                     let mapping = *entry.mapping.lock();
                                                     mapping.filter(|key| key.id == id)
                                                            .map(|key| key.key)
                                                 })
    })
}

/// Read-only values from one checked owner backing, borrowed alongside its
/// mutable state. This is a view of the existing index, not a copied catalog.
pub struct ValuesRead<'a, T, G: InvocationGate>
{
    kind:     KindCode,
    owner:    &'a OwnerRef<G>,
    slots:    &'a [Option<Entry<T, G>>],
    ids:      &'a HashMap<u64, ResourceKey>,
    reserved: &'a [Weak<Reserved<G>>]
}
impl<'a, T, G: InvocationGate> ValuesRead<'a, T, G>
{
    pub fn get(&self, id: u64) -> Option<&'a T>
    {
        let key = self.ids.get(&id).copied().or_else(|| find_reserved(self.reserved, id))?;
        if key.kind() != self.kind || key.owner() != self.owner.owner() || !self.owner.contains(key)
        {
            return None;
        }
        let entry = self.slots.get(key.slot())?.as_ref()?;
        (entry.key == key && entry.id == id).then_some(&entry.value)
    }
}
impl<T, G: InvocationGate> Store<T, G>
{
    pub fn new(kind: KindCode, policy: CapacityPolicy) -> Self
    {
        Self { kind,
               policy,
               owners: Vec::new(),
               ids: HashMap::new(),
               reserved: Vec::new(),
               registration: None,
               storage_registration: None,
               make_state: |_| Ok(()) }
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

    /// A resident manager's values and retained per-owner backing each have a
    /// registered lifetime. The backing namespace must differ from the object
    /// namespace, so bookkeeping never consumes the object's capacity policy.
    pub fn owned(registration: KindLease<G>, storage: KindLease<G>) -> Result<Self, PoolError>
    {
        Self::owned_state(registration, storage, |_| Ok(()))
    }
}
impl<T, G: InvocationGate, S> Store<T, G, S>
{
    /// The manager's saved per-owner state shares the backing's registered
    /// lifetime. Its fallible factory and destructor run outside ledger locks.
    pub fn owned_state(registration: KindLease<G>, storage: KindLease<G>,
                       make_state: fn(DomainId) -> Result<S, PoolError>)
                       -> Result<Self, PoolError>
    {
        if registration.registration().code == storage.registration().code
        {
            return Err(PoolError::DuplicateKind { owner: registration.registration()
                                                                     .release
                                                                     .registration
                                                                     .owner(),
                                                  kind:  registration.registration().code });
        }
        let spec = registration.registration();
        Ok(Self { kind: spec.code,
                  policy: spec.capacity,
                  owners: Vec::new(),
                  ids: HashMap::new(),
                  reserved: Vec::new(),
                  registration: Some(registration),
                  storage_registration: Some(storage),
                  make_state })
    }

    pub fn kind(&self) -> KindCode
    {
        self.kind
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
        self.attach_with(owner, None)
    }

    fn attach_with(&mut self, owner: &OwnerRef<G>, scope: Option<&crate::scope::State>)
                   -> Result<usize, PoolError>
    {
        let at = match self.owner_index(owner.owner())
        {
            Ok(at) => return Ok(at),
            Err(at) => at
        };
        let error = self.storage_error(owner.owner(), 1);
        // This array indexes every owner in one resident store. Per-owner
        // Claims, slots and payload state below retain their caller's charge.
        G::with_system_allocation(|| self.owners.try_reserve(1)).map_err(|_| error)?;
        let mut claim = Arc::try_new(Claim { owner:  owner.clone(),
                                             kind:   self.kind,
                                             active: false }).map_err(|_| error)?;
        let registration = self.registration.as_ref().map(KindLease::id);
        let slots = owner.access(|index| {
                             index.attach_values_with(self.kind, self.policy, registration, scope)
                         })?;
        Arc::get_mut(&mut claim).unwrap().active = true;
        let mut values = Vec::new();
        values.try_reserve(slots).map_err(|_| error)?;
        values.resize_with(slots, || None);
        let state = (self.make_state)(owner.owner())?;
        let storage = self.storage_registration
                          .as_ref()
                          .map(|kind| Storage::new(owner, kind, scope))
                          .transpose()?;
        self.owners.insert(at,
                           Values { claim,
                                    slots: values,
                                    stored: 0,
                                    state: Some(state),
                                    storage: ManuallyDrop::new(storage) });
        Ok(at)
    }

    pub fn insert(&mut self, owner: &OwnerRef<G>, value: T) -> Result<u64, PoolError>
    {
        self.insert_with(owner, |_| value)
    }

    /// Admit registered saved state without publishing a semantic object.
    /// Error/status queries can own state even before their first object exists.
    /// The backing participates in the same walk as object-triggered backing.
    pub fn prepare(&mut self, owner: &OwnerRef<G>) -> Result<(), PoolError>
    {
        if !owner.accepting()
        {
            return Err(PoolError::Stopping { owner: owner.owner() });
        }
        if self.registration.as_ref().is_none_or(|kind| kind.live())
           && self.state(owner.owner().incarnation()).is_some()
        {
            return Ok(());
        }
        let _usage = self.registration.as_ref().map(|kind| kind.acquire(true)).transpose()?;
        self.attach(owner)?;
        Ok(())
    }

    /// Build an object containing its opaque native identity before publishing
    /// it. Construction runs once, outside the owner lock. The live key is
    /// published only after construction succeeds; admission refusal leaves
    /// the value unpublished and drops it outside the metadata lock.
    /// Admission is checked again atomically when the payload is published.
    pub fn insert_with(&mut self, owner: &OwnerRef<G>, make: impl FnOnce(u64) -> T)
                       -> Result<u64, PoolError>
    {
        self.insert_with_receipt(owner, make).map(Publication::id)
    }

    /// Construct once outside the metadata lock, then return ordinary identity
    /// and retained completion provenance in one publication result.
    pub fn insert_with_receipt(&mut self, owner: &OwnerRef<G>, make: impl FnOnce(u64) -> T)
                               -> Result<Publication, PoolError>
    {
        self.insert_value(owner, make, None, None, None, None, None)
    }

    pub(crate) fn insert_projected(&mut self, owner: &OwnerRef<G>, value: T,
                                   projection: crate::projected::ProjectionRelease)
                                   -> Result<Publication, PoolError>
    {
        self.insert_value(owner, |_| value, Some(projection), None, None, None, None)
    }

    /// Publish a scoped root. The store owns its unique close guard; payload
    /// authors only receive the reservation's read-only lease.
    pub fn insert_root(&mut self, reservation: crate::scope::Reservation<G>, value: T)
                       -> Result<u64, PoolError>
    {
        self.insert_root_with_receipt(reservation, |_| value).map(Publication::id)
    }

    pub fn insert_root_with(&mut self, reservation: crate::scope::Reservation<G>,
                            make: impl FnOnce(u64) -> T)
                            -> Result<u64, PoolError>
    {
        self.insert_root_with_receipt(reservation, make).map(Publication::id)
    }

    /// Scoped roots receive the same atomic publication receipt. The store
    /// still owns closure; returning this key never transfers the close guard.
    pub fn insert_root_with_receipt(&mut self, reservation: crate::scope::Reservation<G>,
                                    make: impl FnOnce(u64) -> T)
                                    -> Result<Publication, PoolError>
    {
        self.insert_root_value(reservation, make, None)
    }

    pub(crate) fn insert_root_value(&mut self, reservation: crate::scope::Reservation<G>,
                                    make: impl FnOnce(u64) -> T,
                                    projection: Option<crate::projected::ProjectionRelease>)
                                    -> Result<Publication, PoolError>
    {
        let (owner, root, binding) = reservation.into_parts(self.kind)?;
        self.insert_value(&owner, make, projection, None, Some(root), Some(binding), None)
    }

    pub fn insert_in_scope(&mut self, scope: &crate::scope::Lease<G>, value: T)
                           -> Result<u64, PoolError>
    {
        self.insert_in_scope_with_receipt(scope, |_| value).map(Publication::id)
    }

    /// Existing-operation child admission retains its exact scoped publication
    /// provenance even if the owner walk wins the next metadata handoff.
    pub fn insert_in_scope_with_receipt(&mut self, scope: &crate::scope::Lease<G>,
                                        make: impl FnOnce(u64) -> T)
                                        -> Result<Publication, PoolError>
    {
        self.insert_scoped_value(scope, make, None)
    }

    /// As scoped publication, with a resident lifetime pin kept through the
    /// final real ACK rather than merely through payload destructor entry.
    pub fn insert_in_scope_with_acknowledgement(&mut self, scope: &crate::scope::Lease<G>,
                                                pin: Option<AcknowledgementPin>,
                                                make: impl FnOnce(u64) -> T)
                                                -> Result<Publication, PoolError>
    {
        self.insert_value(scope.owner(), make, None, Some(scope.state()), None, None, pin)
    }

    pub(crate) fn insert_scoped_value(&mut self, scope: &crate::scope::Lease<G>,
                                      make: impl FnOnce(u64) -> T,
                                      projection: Option<crate::projected::ProjectionRelease>)
                                      -> Result<Publication, PoolError>
    {
        self.insert_value(scope.owner(), make, projection, Some(scope.state()), None, None, None)
    }

    /// Prepare while the selected original account can fund metadata. This
    /// performs all allocation; publication/recycling below performs none.
    pub fn reserve_publication(&mut self, owner: &OwnerRef<G>)
                               -> Result<PublicationReservation<G>, PoolError>
    {
        owner.access(|index| index.authorize(self.kind, None))?;
        let at = self.attach(owner)?;
        let error =
            self.storage_error(owner.owner(), self.owners[at].slots.len().saturating_add(1));
        self.reserved.retain(|entry| entry.strong_count() != 0);
        G::with_system_allocation(|| self.reserved.try_reserve(1)).map_err(|_| error)?;
        let completion = crate::completion::Recycling::try_new::<G>().map_err(|_| error)?;
        let slot = loop
        {
            match owner.access(|index| index.reserve_value_slot(self.kind))
            {
                Ok(slot) => break slot,
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
                    owner.access(|index| index.grow_values_with(self.kind, requested, None))?;
                    slots.resize_with(requested, || None);
                }
                Err(error) => return Err(error)
            }
        };
        let inner = Arc::try_new(Reserved { claim: self.owners[at].claim.clone(),
                                            slot,
                                            mapping: Spinlock::new(None),
                                            completion }).map_err(|_| error)?;
        self.reserved.push(Arc::downgrade(&inner));
        Ok(PublicationReservation { inner })
    }

    /// Dedicated publication endpoints still held by their unique publisher or
    /// an in-flight payload. Shared manager indices can preserve this many
    /// buckets while serving ordinary publishers, without a shadow ID count.
    pub fn reserved_capacity(&self) -> usize
    {
        self.reserved.iter().filter(|entry| entry.strong_count() != 0).count()
    }

    pub fn insert_reserved_with_receipt(&mut self, reservation: &mut PublicationReservation<G>,
                                        make: impl FnOnce(u64) -> T)
                                        -> Result<Publication, PublicationError>
    {
        self.insert_reserved(reservation, None, make, None)
    }

    pub fn insert_reserved_in_scope_with_receipt(&mut self,
                                                 reservation: &mut PublicationReservation<G>,
                                                 scope: &crate::scope::Lease<G>,
                                                 make: impl FnOnce(u64) -> T)
                                                 -> Result<Publication, PublicationError>
    {
        self.insert_reserved_in_scope_with_acknowledgement(reservation, scope, None, make)
    }

    pub fn insert_reserved_in_scope_with_acknowledgement(&mut self,
                                                         reservation: &mut PublicationReservation<G>,
                                                         scope: &crate::scope::Lease<G>,
                                                         pin: Option<AcknowledgementPin>,
                                                         make: impl FnOnce(u64) -> T)
                                                         -> Result<Publication, PublicationError>
    {
        if scope.subject() != reservation.inner.claim.owner.owner()
        {
            return Err(PoolError::ForeignOwner { expected: reservation.inner.claim.owner.owner(),
                                                 actual:   scope.subject() }.into());
        }
        self.insert_reserved(reservation, Some(scope.state()), make, pin)
    }

    fn insert_reserved(&mut self, reservation: &mut PublicationReservation<G>,
                       scope: Option<&crate::scope::State>, make: impl FnOnce(u64) -> T,
                       pin: Option<AcknowledgementPin>)
                       -> Result<Publication, PublicationError>
    {
        let inner = &reservation.inner;
        let owner = &inner.claim.owner;
        owner.access(|index| index.authorize(self.kind, scope))?;
        let at = self.owner_index(owner.owner()).map_err(|_| {
                                                     PublicationError::Pool(
            PoolError::ValueStorageAttached { owner: owner.owner(), kind: self.kind })
                                                 })?;
        if !Arc::ptr_eq(&self.owners[at].claim, &inner.claim)
        {
            return Err(PoolError::ValueStorageAttached { owner: owner.owner(),
                                                         kind:  self.kind }.into());
        }
        let reserve =
            inner.completion.take::<G>().ok_or(PublicationError::Busy)?.with_acknowledgement(pin);
        let mut usage = self.registration.as_ref().map(|kind| kind.acquire(true)).transpose()?;
        let id = issue_native_identity().map_err(|_| {
                                            PublicationError::Pool(PoolError::IdentityExhausted)
                                        })?
                                        .get();
        let value = make(id);
        // The constructor/destructor is outside the owner gate. Once the
        // canonical allocation succeeds, publication contains no fallible work.
        let mut payload = Some((value, reserve));
        let key = owner.access(|index| {
                           let key = index.allocate_reserved_value(self.kind, inner.slot, scope)?;
                           debug_assert!(self.owners[at].slots[inner.slot].is_none(),
                                         "a free reserved canonical slot has no retained payload");
                           let (value, reserve) =
                               payload.take().expect("reserved publication consumes its prepared \
                                                      payload exactly once");
                           self.owners[at].slots[inner.slot] =
                               Some(Entry { id,
                                            key,
                                            value,
                                            reserve,
                                            reservation: Some(inner.clone()),
                                            root: None,
                                            registration_use: ManuallyDrop::new(usage.take()) });
                           *inner.mapping.lock() = Some(CompletionKey { id,
                                                                        key });
                           self.owners[at].stored += 1;
                           Ok::<ResourceKey, PoolError>(key)
                       })?;
        Ok(Publication { completion: CompletionKey { id,
                                                     key } })
    }

    fn reserved_key(&self, id: u64) -> Option<ResourceKey>
    {
        find_reserved(&self.reserved, id)
    }

    fn has_reservations(&self, owner: DomainId) -> bool
    {
        self.reserved
            .iter()
            .filter_map(Weak::upgrade)
            .any(|entry| entry.claim.owner.owner() == owner)
    }

    fn insert_value(&mut self, owner: &OwnerRef<G>, make: impl FnOnce(u64) -> T,
                    projection: Option<crate::projected::ProjectionRelease>,
                    scope: Option<&crate::scope::State>, root: Option<crate::scope::Owner<G>>,
                    mut binding: Option<crate::scope::Binding>, pin: Option<AcknowledgementPin>)
                    -> Result<Publication, PoolError>
    {
        owner.access(|index| index.authorize(self.kind, scope))?;
        let mut registration_use =
            self.registration.as_ref().map(|kind| kind.acquire(true)).transpose()?;
        let at = self.attach_with(owner, scope)?;
        let error = self.storage_error(owner.owner(), self.ids.len().saturating_add(1));
        // The cross-owner identity index belongs to the resident store.
        G::with_system_allocation(|| self.ids.try_reserve(1)).map_err(|_| error)?;
        let id = issue_native_identity().map_err(|_| PoolError::IdentityExhausted)?.get();
        let reserve = Reserve::try_new().map_err(|_| error)?
                                        .with_projection(projection)
                                        .with_acknowledgement(pin);
        let mut value = Some((make(id), reserve, root));
        loop
        {
            // Publication is atomic with respect to stop/invalidate/the walk.
            // All backing capacity was reserved before this metadata lock.
            let result = owner.access(|index| {
                             let key = index.allocate_value_with(self.kind, scope)?;
                             debug_assert!(self.owners[at].slots[key.slot()].is_none());
                             let (value, reserve, root) =
                                 value.take().expect("unpublished value is consumed only by \
                                                      successful publication");
                             if let Some(binding) = binding.take()
                             {
                                 binding.publish(key);
                             }
                             self.owners[at].slots[key.slot()] =
                                 Some(Entry { id,
                                              key,
                                              value,
                                              reserve,
                                              reservation: None,
                                              root,
                                              registration_use:
                                                  ManuallyDrop::new(registration_use.take()) });
                             self.ids.insert(id, key);
                             self.owners[at].stored += 1;
                             Ok(key)
                         });
            match result
            {
                Ok(key) =>
                {
                    return Ok(Publication { completion: CompletionKey { id,
                                                                        key } });
                }
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
                    owner.access(|index| index.grow_values_with(self.kind, requested, scope))?;
                    slots.resize_with(requested, || None);
                }
                Err(error) => return Err(error)
            }
        }
    }

    fn locate(&self, id: u64) -> Option<(usize, ResourceKey)>
    {
        let key = self.ids.get(&id).copied().or_else(|| self.reserved_key(id))?;
        let at = self.owner_index(key.owner()).ok()?;
        self.owners[at].claim.owner.contains(key).then_some((at, key))
    }

    pub fn get(&self, id: u64) -> Option<&T>
    {
        self.get_with_owner(id).map(|(_, value)| value)
    }

    /// Read a live payload and its authenticated owner from one validation.
    /// The returned identity is metadata, not authority to mutate its ledger.
    pub fn get_with_owner(&self, id: u64) -> Option<(DomainId, &T)>
    {
        if self.registration.as_ref().is_some_and(|kind| !kind.live())
        {
            return None;
        }
        let (at, key) = self.locate(id)?;
        let entry = self.owners[at].slots.get(key.slot())?.as_ref()?;
        (entry.key == key && entry.id == id).then_some((key.owner(), &entry.value))
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

    pub fn completion_key(&self, id: u64) -> Option<CompletionKey>
    {
        Some(CompletionKey { id,
                             key: self.key(id)? })
    }

    /// An authenticated late completion may inspect its retained immutable
    /// payload even while the ordinary ID is invalid or quarantined. A key from
    /// another store/owner/registration, or a completed/reused slot, cannot alias.
    /// No mutable payload, ticket or owner lock escapes this read.
    pub fn completing(&self, completion: &CompletionKey) -> Option<&T>
    {
        let key = completion.key;
        if key.kind() != self.kind
        {
            return None;
        }
        let at = self.owner_index(key.owner()).ok()?;
        let entry = self.owners[at].slots.get(key.slot())?.as_ref()?;
        (entry.key == key && entry.id == completion.id).then_some(&entry.value)
    }

    /// Begin ordinary manager disposal without moving its retained payload.
    /// The returned unique ticket must be completed or retained for retry.
    pub fn begin_release(&self, id: u64) -> Option<ReleasedResource>
    {
        let (at, key) = self.locate(id)?;
        self.owners[at].claim.owner.invalidate(key).ok()
    }

    /// Keep a refused explicit disposal in the same preallocated quarantine
    /// used by the owner walk. Foreign tickets remain owned by the error.
    pub fn quarantine(&self, ticket: ReleasedResource) -> Result<(), CompletionError>
    {
        let key = ticket.key();
        if self.releasing(&ticket).is_none()
        {
            return Err(CompletionError::stale(ticket));
        }
        let Ok(at) = self.owner_index(key.owner())
        else
        {
            return Err(CompletionError::stale(ticket));
        };
        self.owners[at].claim.owner.quarantine(ticket)
    }

    /// Abandonment is not a protocol acknowledgement. Invalidate ordinary
    /// identities and retain their payloads in the owner's existing quarantine.
    /// The unique walk may later retry; dropping this store cannot free them.
    pub fn quarantine_live(&self)
    {
        for values in &self.owners
        {
            for entry in values.slots.iter().flatten()
            {
                // Visit each retained slot once. Restarting ordinary iteration
                // after each invalidation would rescan every prior dead slot.
                let Ok(ticket) = values.claim.owner.invalidate(entry.key)
                else
                {
                    continue;
                };
                if let Err(error) = values.claim.owner.quarantine(ticket)
                {
                    // An invalidated payload still owns its barrier even if a
                    // damaged ledger cannot accept the original retry ticket.
                    core::mem::forget(error);
                }
            }
        }
    }

    pub fn remove(&mut self, id: u64) -> Option<Removed<T, G>>
    {
        let (at, key) = self.locate(id)?;
        let entry = self.owners[at].slots.get_mut(key.slot())?.take()?;
        match self.owners[at].claim.owner.invalidate(key)
        {
            Ok(released) => Some(self.take(at, released, entry)),
            Err(_) =>
            {
                // The walk won invalidation; its ticket still owns this exact
                // payload. Restore it for that dispatcher without destruction.
                self.owners[at].slots[key.slot()] = Some(entry);
                None
            }
        }
    }

    fn take(&mut self, at: usize, released: ReleasedResource, entry: Entry<T, G>) -> Removed<T, G>
    {
        self.owners[at].stored -= 1;
        if let Some(reservation) = &entry.reservation
        {
            G::run(|| *reservation.mapping.lock() = None);
        }
        else
        {
            self.ids.remove(&entry.id);
        }
        let usage = ManuallyDrop::into_inner(entry.registration_use);
        let removed =
            Removed { id:           entry.id,
                      value:        Some(entry.value),
                      root:         entry.root,
                      _reservation: entry.reservation,
                      completion:
                          entry.reserve
                               .released(self.owners[at].claim.owner.clone(), released, usage),
                      _claim:       self.owners[at].claim.clone() };
        if self.owners[at].stored == 0
           && !self.has_reservations(self.owners[at].claim.owner.owner())
           && self.owners[at].storage.as_ref().is_some_and(|state| state.released)
        {
            self.owners.remove(at);
        }
        removed
    }

    /// Accept the backing's invalidate-first ticket. If payloads remain, the
    /// death barrier stays pending and the last payload removal frees backing.
    /// Quarantined payloads therefore retain the backing until a real retry.
    pub fn release_storage(&mut self, ticket: ReleasedResource) -> Result<(), CompletionError>
    {
        let key = ticket.key();
        if let Ok(at) = self.owner_index(key.owner())
        {
            if let Some(state) = &mut *self.owners[at].storage
            {
                if state.key == key && !state.released
                {
                    state.completion.receive(ticket)?;
                    state.released = true;
                    if self.owners[at].stored == 0 && !self.has_reservations(key.owner())
                    {
                        self.owners.remove(at);
                    }
                    return Ok(());
                }
            }
        }
        Err(CompletionError::stale(ticket))
    }

    /// Inspect a retained payload through the unique invalidate-first cleanup
    /// capability. Ordinary IDs remain invalid throughout refusal and retry.
    /// No mutable payload or metadata lock escapes to the callback.
    pub fn releasing(&self, ticket: &ReleasedResource) -> Option<&T>
    {
        let key = ticket.key();
        let at = self.owner_index(key.owner()).ok()?;
        if key.kind() != self.kind
        {
            return None;
        }
        let entry = self.owners[at].slots.get(key.slot())?.as_ref()?;
        (entry.key == key).then_some(&entry.value)
    }

    /// Mutate copy-only cleanup state without revalidating an ordinary ID.
    /// Non-Copy payloads can own linear resources; their destruction must stay
    /// behind `release` and its guard. They cannot be extracted or replaced
    /// through this callback. No metadata lock is held during the operation.
    /// ```compile_fail
    /// use mac_object_table::{domain::ReleasedResource, shared::InvocationGate, store::Store};
    /// fn steal<G: InvocationGate>(store: &mut Store<String, G>, ticket: &ReleasedResource) {
    ///     let owned = store.with_releasing(ticket, core::mem::take);
    /// }
    /// ```
    pub fn with_releasing<R>(&mut self, ticket: &ReleasedResource,
                             operation: impl FnOnce(&mut T) -> R)
                             -> Option<R>
        where T: Copy
    {
        let key = ticket.key();
        let at = self.owner_index(key.owner()).ok()?;
        if key.kind() != self.kind
        {
            return None;
        }
        let entry = self.owners[at].slots.get_mut(key.slot())?.as_mut()?;
        (entry.key == key).then(|| operation(&mut entry.value))
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
                if let Some(entry) =
                    self.owners[at].slots.get_mut(key.slot()).and_then(Option::take)
                {
                    return Ok(self.take(at, released, entry));
                }
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

    /// Explicit live-owner disposal may free an empty cache too. Never destroy
    /// quarantined payloads or acknowledge another dispatcher's pending ticket.
    fn prune_empty_owner(&mut self, incarnation: u64) -> bool
    {
        let Ok(at) =
            self.owners.binary_search_by_key(&incarnation, |values| {
                           values.claim.owner.owner().incarnation()
                       })
        else
        {
            return true;
        };
        if self.owners[at].stored != 0 || self.has_reservations(self.owners[at].claim.owner.owner())
        {
            return false;
        }
        self.owners.remove(at);
        true
    }

    /// Backing reservation count, including invalidated payloads awaiting their
    /// unique cleanup dispatcher. This is not an ordinary live-ID query.
    pub(crate) fn stored_len(&self) -> usize
    {
        self.owners.iter().map(|owner| owner.stored).sum()
    }

    pub fn iter(&self) -> impl Iterator<Item = (u64, &T)>
    {
        self.owners.iter().flat_map(Values::iter)
    }

    /// Trusted retained metadata, including invalidated payloads awaiting their
    /// unique release ticket. This does not validate any ordinary object ID.
    pub fn retained_values(&self) -> impl Iterator<Item = &T>
    {
        self.owners.iter().flat_map(|owner| owner.slots.iter().flatten().map(|entry| &entry.value))
    }

    /// Borrow only live payloads. Validation finishes before the caller can
    /// mutate a value; no owner lock or temporary identity array is retained.
    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut T>
    {
        let admitting = self.registration.as_ref().is_none_or(|kind| kind.live());
        self.owners.iter_mut().flat_map(move |values| {
                                  let owner = &values.claim.owner;
                                  values.slots.iter_mut().filter_map(move |slot| {
                let entry = slot.as_mut()?;
                (admitting && owner.contains(entry.key)).then_some(&mut entry.value)
            })
                              })
    }

    /// Saved state is accessible only while its registered backing is live.
    /// Pending/quarantined backing stays retained exclusively for cleanup.
    pub fn state(&self, incarnation: u64) -> Option<&S>
    {
        let at =
            self.owners
                .binary_search_by_key(&incarnation, |values| {
                    values.claim.owner.owner().incarnation()
                })
                .ok()?;
        if !self.storage_registration.as_ref()?.live()
        {
            return None;
        }
        let values = &self.owners[at];
        let storage = values.storage.as_ref()?;
        values.claim.owner.contains(storage.key).then_some(())?;
        values.state.as_ref()
    }

    pub fn state_mut(&mut self, incarnation: u64) -> Option<&mut S>
    {
        let at =
            self.owners
                .binary_search_by_key(&incarnation, |values| {
                    values.claim.owner.owner().incarnation()
                })
                .ok()?;
        if !self.storage_registration.as_ref()?.live()
        {
            return None;
        }
        let values = &mut self.owners[at];
        let storage = values.storage.as_ref()?;
        values.claim.owner.contains(storage.key).then_some(())?;
        values.state.as_mut()
    }

    /// Split safe borrows of live registered state and its owner's values.
    /// No payload mutation, cleanup authority or metadata lock escapes here.
    pub fn state_values(&mut self, incarnation: u64) -> Option<(&mut S, ValuesRead<'_, T, G>)>
    {
        let at =
            self.owners
                .binary_search_by_key(&incarnation, |values| {
                    values.claim.owner.owner().incarnation()
                })
                .ok()?;
        if !self.storage_registration.as_ref()?.live()
        {
            return None;
        }
        let values = &mut self.owners[at];
        let storage = values.storage.as_ref()?;
        let owner = &values.claim.owner;
        owner.contains(storage.key).then_some(())?;
        Some((values.state.as_mut()?,
              ValuesRead { kind: self.kind,
                           owner,
                           slots: &values.slots,
                           ids: &self.ids,
                           reserved: &self.reserved }))
    }

    /// No temporary ID vector and no allocation proportional to object count.
    pub fn remove_where(&mut self, mut predicate: impl FnMut(DomainId, &T) -> bool)
    {
        self.remove_where_with(&mut predicate, drop);
    }

    /// Apply manager bookkeeping to each successfully removed payload, outside
    /// owner locks and without a temporary object-ID allocation.
    pub fn remove_where_with(&mut self, mut predicate: impl FnMut(DomainId, &T) -> bool,
                             mut removed: impl FnMut(Removed<T, G>))
    {
        let mut at = 0;
        while at < self.owners.len()
        {
            let owner = self.owners[at].claim.owner.owner();
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
                    if let Some(object) = self.remove(id)
                    {
                        removed(object);
                        if self.owners
                               .get(at)
                               .is_none_or(|values| values.claim.owner.owner() != owner)
                        {
                            break;
                        }
                    }
                }
            }
            if self.owners.get(at).is_some_and(|values| values.claim.owner.owner() == owner)
            {
                at += 1;
            }
        }
    }
}
impl<T, G: InvocationGate, S> Drop for Store<T, G, S>
{
    fn drop(&mut self)
    {
        self.remove_where(|_, _| true);
        for values in self.owners.drain(..)
        {
            if values.stored != 0
            {
                // Abandonment is not cleanup. Preserve quarantined payloads,
                // their provider pins and backing, rather than running their
                // destructors without the walk's unique tickets.
                core::mem::forget(values);
            }
        }
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
    id:           u64,
    value:        Option<T>,
    root:         Option<crate::scope::Owner<G>>,
    completion:   Completion<G>,
    _claim:       Arc<Claim<G>>,
    _reservation: Option<Arc<Reserved<G>>>
}
impl<T, G: InvocationGate> Removed<T, G>
{
    /// Read only through the original admission receipt after invalidation.
    /// Matching an ID alone never upgrades a removal guard into live authority.
    pub fn completing(&self, receipt: &CompletionKey) -> Option<&T>
    {
        (receipt.id == self.id && receipt.key == self.completion.key()).then(|| self.value.as_ref())
                                                                       .flatten()
    }

    /// Ordinary identity metadata; removal has already invalidated it.
    pub fn id(&self) -> u64
    {
        self.id
    }
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
        self.completion.run(|| {
                           drop(self.value.take());
                           drop(self.root.take());
                       });
    }
}
