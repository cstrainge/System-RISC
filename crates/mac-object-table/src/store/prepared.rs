//! Ordinary O(1)-index publication with externally prepared capacity. This is
//! deliberately separate from the bounded emergency reserved-index bucket.
use core::{marker::PhantomData,
           sync::atomic::{AtomicBool, AtomicUsize, Ordering}};

use super::*;
use crate::domain::{DomainSlots, LedgerCapacityPlan, PoolRequest, PreparedLedgerCapacity};

pub(super) struct Incarnation
{
    live:    AtomicBool,
    credits: AtomicUsize
}
impl Incarnation
{
    pub(super) fn live(&self) -> bool
    {
        self.live.load(Ordering::Acquire)
    }

    pub(super) fn credits(&self) -> usize
    {
        self.credits.load(Ordering::Acquire)
    }

    pub(super) fn close(&self)
    {
        self.live.store(false, Ordering::Release);
    }

    fn release(&self, claim: &Claim<impl InvocationGate>)
    {
        // A duplicate/lost permit is structural uncertainty. Stop new prepared
        // publication; never wrap a counter or fabricate an object completion.
        if self.credits
               .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
               .is_err()
           || claim.preparing
                   .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                   .is_err()
        {
            self.close();
        }
    }
}

/// Capacity observations plus real owner/registration pins. Preparing this
/// value invokes no payload constructor under Store/owner guards.
pub struct PublicationPlan<T, G: InvocationGate, S = ()>
{
    owner:                OwnerRef<G>,
    kind:                 KindCode,
    policy:               CapacityPolicy,
    registration:         Option<KindLease<G>>,
    storage_registration: Option<KindLease<G>>,
    claim:                Option<Arc<Claim<G>>>,
    incarnation:          Arc<Incarnation>,
    owners_capacity:      usize,
    ids_capacity:         usize,
    values_slots:         usize,
    ledger:               LedgerCapacityPlan,
    make_state:           fn(DomainId) -> Result<S, PoolError>,
    value:                PhantomData<fn() -> T>
}
/// All buffers, completion storage and registration pins already allocated.
/// This is not a reserved slot, live birth or object/owner authorization.
pub struct PreparedPublication<T, G: InvocationGate, S = ()>
{
    owner:                OwnerRef<G>,
    kind:                 KindCode,
    registration:         Option<KindLease<G>>,
    storage_registration: Option<KindLease<G>>,
    usage:                Option<Use>,
    storage_usage:        Option<Use>,
    claim:                Option<Arc<Claim<G>>>,
    attach:               bool,
    incarnation:          Arc<Incarnation>,
    owners:               Vec<Values<T, G, S>>,
    ids:                  HashMap<u64, ResourceKey>,
    values:               Vec<Option<Entry<T, G>>>,
    state:                Option<S>,
    ledger:               PreparedLedgerCapacity,
    id:                   u64,
    reserve:              Option<Reserve>,
    storage_reserve:      Option<Reserve>
}
/// Return this owner outside the Store/owner gate before destruction. It holds
/// obsolete capacity and unused preparation; never a caller's published value.
#[must_use]
pub struct RetiredCapacity<T, G: InvocationGate, S = ()>
{
    preparation: PreparedPublication<T, G, S>
}
/// One canonical removal, then final owner backing. RAII disposal order is
/// object payload/completion first, array/state/Claim/backing completion second.
#[must_use]
pub struct Removal<T, G: InvocationGate, S = ()>
{
    pub(super) object:  Removed<T, G>,
    pub(super) backing: Option<RetiredOwner<T, G, S>>
}
impl<T, G: InvocationGate, S> Deref for Removal<T, G, S>
{
    type Target = Removed<T, G>;

    fn deref(&self) -> &Self::Target
    {
        &self.object
    }
}
impl<T, G: InvocationGate, S> Removal<T, G, S>
{
    pub fn dispose(self)
    {
        drop(self);
    }
}

/// Already-released owner storage. Actual backing destruction and its retained
/// canonical ACK happen when this owner is disposed OUTSIDE manager guards.
#[must_use]
pub struct RetiredOwner<T, G: InvocationGate, S = ()>
{
    pub(super) values: Values<T, G, S>
}
impl<T, G: InvocationGate, S> RetiredOwner<T, G, S>
{
    pub fn dispose(self)
    {
        drop(self.values);
    }
}
impl<T, G: InvocationGate, S> RetiredCapacity<T, G, S>
{
    /// Call only after the manager/Store/owner guard has ended.
    pub fn dispose(self)
    {
        drop(self.preparation);
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreparedError
{
    /// Another publisher consumed the observed capacity. Replan outside guards.
    CapacityChanged,
    /// Numeric kind/owner equality cannot substitute for this exact Store.
    WrongStore,
    RetiredStore,
    Pool(PoolError)
}
impl From<PoolError> for PreparedError
{
    fn from(cause: PoolError) -> Self
    {
        match cause
        {
            PoolError::GrowthRequired { .. } => Self::CapacityChanged,
            other => Self::Pool(other)
        }
    }
}
impl core::fmt::Display for PreparedError
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::CapacityChanged =>
            {
                out.write_str("ordinary publication capacity changed; replan outside metadata \
                               guards")
            }
            Self::WrongStore =>
            {
                out.write_str("ordinary publication belongs to a different Store incarnation or \
                               registration")
            }
            Self::RetiredStore =>
            {
                out.write_str("ordinary publication Store is retired or its preparation \
                               accounting is uncertain")
            }
            Self::Pool(cause) => cause.fmt(out)
        }
    }
}
pub struct AdmissionFailure<P>
{
    pub cause:       PreparedError,
    pub preparation: P
}
pub struct PublishFailure<P, T>
{
    pub cause:  PreparedError,
    pub permit: P,
    pub value:  T
}
/// Spent permit/provider pins; dispose only after the publication guard ends.
#[must_use]
pub struct RetiredPublication<G: InvocationGate>
{
    permit: PublicationPermit<G>
}
impl<G: InvocationGate> RetiredPublication<G>
{
    pub fn dispose(self)
    {
        drop(self.permit);
    }
}

/// Non-Clone ordinary publication capacity in one actual Store incarnation.
/// Its Claim and registration Use survive Store teardown. A recreated Store
/// cannot consume it; Drop cancels only an unpublished slot/credit, never ACKs.
pub struct PublicationPermit<G: InvocationGate>
{
    incarnation: Arc<Incarnation>,
    claim:       Arc<Claim<G>>,
    slot:        Option<usize>,
    id:          u64,
    reserve:     Option<Reserve>,
    usage:       Option<Use>,
    credited:    bool,
    // LAST: notify after the actual Arc/Reserve/provider pins above decrement.
    _released:   PublicationReleased<G>
}
struct PublicationReleased<G: InvocationGate>
{
    owner:     DomainId,
    claim:     Option<Arc<Claim<G>>>,
    admission: Option<Arc<AtomicBool>>
}
impl<G: InvocationGate> Drop for PublicationReleased<G>
{
    fn drop(&mut self)
    {
        let retiring =
            self.claim.as_ref().is_some_and(|claim| claim.retiring.load(Ordering::Acquire));
        // Last owner-Kernel Claim pin drops before deciding whether a stop/walk
        // needs progress. Admission itself is only foundation control metadata.
        drop(self.claim.take());
        let stopping = self.admission.as_ref().is_some_and(|flag| !flag.load(Ordering::Acquire));
        drop(self.admission.take());
        if retiring || stopping
        {
            G::publication_released(self.owner);
        }
    }
}
impl<G: InvocationGate> PublicationPermit<G>
{
    /// Identity already issued by this exact canonical preparation. Observing
    /// it grants no publication or lookup authority; only consuming this permit
    /// can publish its associated object. Useful for an object's own immutable ID.
    pub fn id(&self) -> u64
    {
        self.id
    }
}
impl<G: InvocationGate> Drop for PublicationPermit<G>
{
    fn drop(&mut self)
    {
        if let Some(slot) = self.slot.take()
        {
            self.claim.owner.access(|index| index.unreserve_value_slot(self.claim.kind, slot));
        }
        if self.credited
        {
            self.credited = false;
            self.incarnation.release(&self.claim);
        }
        // Reserve and provider/Claim pins drop only after the owner turn ends.
    }
}

impl<T, G: InvocationGate> Store<T, G>
{
    /// Construct the real Store and its immutable preparation identity outside
    /// every manager/heap/index guard. No plan can precede this identity.
    pub fn owned_prepared(registration: KindLease<G>, storage: KindLease<G>)
                          -> Result<Self, PoolError>
    {
        let descriptor = registration.registration();
        let error = PoolError::Storage { owner:           descriptor.release.registration.owner(),
                                         kind:            descriptor.code,
                                         used:            0,
                                         requested_slots: 1,
                                         limit:           descriptor.capacity.limit() };
        let mut store = Self::owned(registration, storage)?;
        store.incarnation = Some(G::with_system_allocation(|| {
                                     Arc::try_new(Incarnation { live:    AtomicBool::new(true),
                                                                credits: AtomicUsize::new(0) })
                                 }).map_err(|_| error)?);
        Ok(store)
    }
}

impl<T, G: InvocationGate, S> Store<T, G, S>
{
    /// Observation for registered backing cleanup. A provider may retain its
    /// original ticket until the final real unpublished permit ends, instead
    /// of escrowing it where no semantic object exists to trigger disposal.
    pub fn publication_preparations(&self, owner: DomainId) -> usize
    {
        self.owner_index(owner)
            .ok()
            .map_or(0, |at| self.owners[at].claim.preparing.load(Ordering::Acquire))
    }

    /// Targeted per-change completion, including owner walk that received the
    /// backing ticket before the final ordinary permit released its slot.
    pub fn reap_publication_capacity(&mut self, owner: DomainId) -> Option<RetiredOwner<T, G, S>>
    {
        let at = self.owner_index(owner).ok()?;
        if self.owners[at].stored != 0
           || self.has_reservations(owner)
           || !self.owners[at].storage.as_ref().is_some_and(|s| s.released)
        {
            return None;
        }
        Some(RetiredOwner { values: self.owners.remove(at) })
    }

    pub fn plan_publication(&self, owner: &OwnerRef<G>)
                            -> Result<PublicationPlan<T, G, S>, PoolError>
    {
        let incarnation = self.incarnation.as_ref().ok_or(PoolError::InvalidGrowth)?;
        if !incarnation.live()
        {
            return Err(PoolError::InvalidGrowth);
        }
        let at = self.owner_index(owner.owner()).ok();
        let attach = at.is_none();
        let request = PoolRequest { kind: self.kind,
                                    policy: self.policy,
                                    registration: self.registration.as_ref().map(KindLease::id),
                                    attach };
        let storage = if attach
        {
            self.storage_registration.as_ref().map(|kind| {
                                                  let spec = kind.registration();
                                                  PoolRequest { kind:         spec.code,
                                                                policy:       spec.capacity,
                                                                registration: Some(kind.id()),
                                                                attach:       true }
                                              })
        }
        else
        {
            None
        };
        let ledger = owner.access(|index| index.plan_value_capacity([Some(request), storage]))?;
        let slots = ledger.slots();
        let values_slots =
            if at.is_none_or(|at| self.owners[at].slots.len() < slots) { slots } else { 0 };
        let owners_required =
            self.owners.len().checked_add(usize::from(attach)).ok_or(PoolError::InvalidGrowth)?;
        let owners_capacity =
            if self.owners.capacity() < owners_required { owners_required } else { 0 };
        let credits = self.incarnation.as_ref().map_or(0, |s| s.credits());
        let ids_required = self.ids
                               .len()
                               .checked_add(credits)
                               .and_then(|n| n.checked_add(1))
                               .ok_or(PoolError::InvalidGrowth)?;
        let ids_capacity = if self.ids.capacity() < ids_required { ids_required } else { 0 };
        Ok(PublicationPlan { owner: owner.clone(),
                             kind: self.kind,
                             policy: self.policy,
                             registration: self.registration.clone(),
                             storage_registration: self.storage_registration.clone(),
                             claim: at.map(|at| self.owners[at].claim.clone()),
                             incarnation: incarnation.clone(),
                             owners_capacity,
                             ids_capacity,
                             values_slots,
                             ledger,
                             make_state: self.make_state,
                             value: PhantomData })
    }

    /// No allocation, callback or destructor runs here. Refusal retains the same
    /// preparation, including any obsolete capacity moved into it before refusal.
    pub fn admit_prepared(
        &mut self, mut preparation: PreparedPublication<T, G, S>)
        -> Result<(PublicationPermit<G>, RetiredCapacity<T, G, S>),
                  AdmissionFailure<PreparedPublication<T, G, S>>>
    {
        match self.install_publication(&mut preparation)
        {
            Ok(slot) =>
            {
                let claim =
                    preparation.claim.take().expect("prepared admission retains the exact \
                                                     validated Claim until permit transfer");
                let permit =
                    PublicationPermit { incarnation: preparation.incarnation.clone(),
                                        claim:claim.clone(),
                                        slot: Some(slot),
                                        id: preparation.id,
                                        reserve: preparation.reserve.take(),
                                        usage: preparation.usage.take(),
                                        credited: true,
                                        _released: PublicationReleased { owner: preparation.owner
                                                                                           .owner(),
                                                                         claim:Some(claim),
                                                                         admission:Some(preparation.owner.publication_admission()) } };
                Ok((permit, RetiredCapacity { preparation }))
            }
            Err(cause) =>
            {
                Err(AdmissionFailure { cause,
                                       preparation })
            }
        }
    }

    fn install_publication(&mut self, preparation: &mut PreparedPublication<T, G, S>)
                           -> Result<usize, PreparedError>
    {
        if !preparation.incarnation.live()
        {
            return Err(PreparedError::RetiredStore);
        }
        if preparation.kind != self.kind
           || preparation.registration.as_ref().map(KindLease::id)
              != self.registration.as_ref().map(KindLease::id)
           || preparation.storage_registration.as_ref().map(KindLease::id)
              != self.storage_registration.as_ref().map(KindLease::id)
        {
            return Err(PreparedError::WrongStore);
        }
        for kind in
            [self.registration.as_ref(), self.storage_registration.as_ref()].into_iter().flatten()
        {
            if !kind.live()
            {
                let descriptor = kind.registration();
                return Err(PoolError::RegistrationUnavailable {
                    kind: descriptor.code, registration: descriptor.release.registration
                }.into());
            }
        }
        if self.incarnation
               .as_ref()
               .is_none_or(|state| !Arc::ptr_eq(state, &preparation.incarnation))
        {
            return Err(PreparedError::WrongStore);
        }
        if preparation.reserve.is_none()
        {
            return Err(PoolError::InvalidGrowth.into());
        }
        let at = self.owner_index(preparation.owner.owner());
        if at.is_err() != preparation.attach
        {
            return Err(PreparedError::CapacityChanged);
        }
        let claim = preparation.claim.as_mut().ok_or(PoolError::InvalidGrowth)?;
        if let Ok(at) = at
        {
            if !Arc::ptr_eq(claim, &self.owners[at].claim)
            {
                return Err(PreparedError::CapacityChanged);
            }
        }
        else if Arc::get_mut(claim).is_none()
                  || preparation.state.is_none()
                  || self.storage_registration.is_some()
                     && (preparation.storage_reserve.is_none()
                         || preparation.storage_usage.is_none())
        {
            return Err(PoolError::InvalidGrowth.into());
        }
        if claim.preparing.load(Ordering::Acquire) == usize::MAX
        {
            return Err(PoolError::InvalidGrowth.into());
        }
        let credits = preparation.incarnation.credits();
        let required = self.ids
                           .len()
                           .checked_add(credits)
                           .and_then(|n| n.checked_add(1))
                           .ok_or(PoolError::InvalidGrowth)?;
        if self.ids.capacity() < required && preparation.ids.capacity() < required
        {
            return Err(PreparedError::CapacityChanged);
        }
        let owners_required = self.owners
                                  .len()
                                  .checked_add(usize::from(preparation.attach))
                                  .ok_or(PoolError::InvalidGrowth)?;
        if self.owners.capacity() < owners_required
           && preparation.owners.capacity() < owners_required
        {
            return Err(PreparedError::CapacityChanged);
        }
        let ledger_slots = preparation.owner.access(|index| {
                                                 index.authorize(self.kind, None)?;
                                                 Ok::<_, PoolError>(index.pools()
                                                          .find(|p| p.kind() == self.kind)
                                                          .map_or(0, DomainSlots::storage_slots))
                                             })?;
        let target_slots = ledger_slots.max(preparation.ledger.slots());
        let current_slots = at.ok().map_or(0, |at| self.owners[at].slots.len());
        if current_slots < target_slots && preparation.values.len() < target_slots
        {
            return Err(PreparedError::CapacityChanged);
        }
        // All capacities checked first. Moves preserve owning values; every
        // old buffer is empty/vacant and returned in preparation for outside Drop.
        if self.ids.capacity() < required
        {
            for (id, key) in self.ids.drain()
            {
                preparation.ids.insert(id, key);
            }
            core::mem::swap(&mut self.ids, &mut preparation.ids);
        }
        if self.owners.capacity() < owners_required
        {
            for values in self.owners.drain(..)
            {
                preparation.owners.push(values);
            }
            core::mem::swap(&mut self.owners, &mut preparation.owners);
        }
        if let Ok(at) = at
        {
            if current_slots < target_slots
            {
                for (to, from) in preparation.values.iter_mut().zip(&mut self.owners[at].slots)
                {
                    *to = from.take();
                }
                core::mem::swap(&mut self.owners[at].slots, &mut preparation.values);
            }
        }
        let (slot, storage_key) =
            preparation.owner.access(|index| {
                                  if !preparation.ledger.directory_fits(index)?
                                  {
                                      return Err(PreparedError::CapacityChanged);
                                  }
                                  preparation.ledger.install(index)?;
                                  preparation.ledger.admit(index).map_err(PreparedError::from)
                              })?;
        if let Err(at) = at
        {
            Arc::get_mut(claim).expect("unpublished Claim is unique before its sole attachment")
                               .active = true;
            let storage =
                storage_key.map(|key| {
                               let kind =
                                   preparation.storage_registration
                                              .as_ref()
                                              .expect("prepared backing birth has its pinned \
                                                       storage registration");
                               Storage { _claim: Claim { owner:     preparation.owner.clone(),
                                                         kind:      kind.registration().code,
                                                         active:    true,
                                                         preparing: AtomicUsize::new(0),
                                                         retiring:  AtomicBool::new(false) },
                                         key,
                                         released: false,
                                         completion:
                                             preparation.storage_reserve
                                                        .take()
                                                        .expect("prepared backing birth retains \
                                                                 prepaid completion storage")
                                                        .live(preparation.owner.clone(),
                                                              key,
                                                              preparation.storage_usage.take()) }
                           });
            let values = Values { slots:   core::mem::take(&mut preparation.values),
                                  claim:   ClaimPin::new(claim.clone()),
                                  stored:  0,
                                  state:   preparation.state.take(),
                                  storage: ManuallyDrop::new(storage) };
            self.owners.insert(at, values);
        }
        preparation.incarnation.credits.fetch_add(1, Ordering::AcqRel);
        claim.preparing.fetch_add(1, Ordering::AcqRel);
        Ok(slot)
    }

    pub fn publish_prepared(
        &mut self, mut permit: PublicationPermit<G>, value: T)
        -> Result<(Publication, RetiredPublication<G>), PublishFailure<PublicationPermit<G>, T>>
    {
        // Observe every Store-side refusal while the original payload remains
        // owned separately. Only canonical admission below can consume it.
        let checked: Result<(usize, usize), PreparedError> = (|| {
            let incarnation = self.incarnation.as_ref().ok_or(PoolError::InvalidGrowth)?;
            if !incarnation.live()
            {
                return Err(PreparedError::RetiredStore);
            }
            if !Arc::ptr_eq(incarnation, &permit.incarnation)
            {
                return Err(PreparedError::WrongStore);
            }
            if !permit.credited || permit.reserve.is_none() || self.ids.contains_key(&permit.id)
            {
                return Err(PoolError::InvalidGrowth.into());
            }
            if let Some(kind) = self.registration.as_ref().filter(|kind| !kind.live())
            {
                let descriptor = kind.registration();
                return Err(PoolError::RegistrationUnavailable {
                    kind:descriptor.code,registration:descriptor.release.registration
                }.into());
            }
            let credits = incarnation.credits();
            if credits == 0
               || self.ids.capacity()
                  < self.ids.len().checked_add(credits).ok_or(PoolError::InvalidGrowth)?
            {
                return Err(PoolError::InvalidGrowth.into());
            }
            let at = self.owner_index(permit.claim.owner.owner())
                         .map_err(|_| PoolError::InvalidGrowth)?;
            if !Arc::ptr_eq(&self.owners[at].claim, &permit.claim)
            {
                return Err(PoolError::InvalidGrowth.into());
            }
            let slot = permit.slot.ok_or(PoolError::InvalidGrowth)?;
            if self.owners[at].slots.get(slot).is_none_or(Option::is_some)
            {
                return Err(PoolError::InvalidGrowth.into());
            }
            Ok((at, slot))
        })();
        let (at, slot) = match checked
        {
            Ok(checked) => checked,
            Err(cause) =>
            {
                return Err(PublishFailure { cause,
                                            permit,
                                            value });
            }
        };
        let Some(reserve) = permit.reserve.take()
        else
        {
            return Err(PublishFailure { cause: PoolError::InvalidGrowth.into(),
                                        permit,
                                        value });
        };
        // The ledger turn returns the SAME payload and completion reserve on
        // refusal. Success is the only branch which moves either into Entry;
        // nothing fallible follows the canonical birth inside this turn.
        let admitted = permit.claim.owner.access(|index| {
                                             match index.allocate_reserved_value(self.kind, slot,
                                                                                 None)
                                             {
                                                 Err(cause) => Err((cause, value, reserve)),
                                                 Ok(key) =>
                                                 {
                                                     let entry = Entry { id: permit.id,
                                                                         key,
                                                                         value,
                                                                         reserve,
                                                                         reservation: None,
                                                                         root: None,
                                                                         registration_use: ManuallyDrop::new(permit.usage.take()) };
                                                     self.owners[at].slots[slot] = Some(entry);
                                                     self.ids.insert(permit.id, key);
                                                     self.owners[at].stored += 1;
                                                     index.unreserve_value_slot(self.kind, slot);
                                                     Ok(key)
                                                 }
                                             }
                                         });
        let key = match admitted
        {
            Ok(key) => key,
            Err((cause, value, reserve)) =>
            {
                permit.reserve = Some(reserve);
                return Err(PublishFailure { cause: cause.into(),
                                            permit,
                                            value });
            }
        };
        permit.slot = None;
        permit.credited = false;
        permit.incarnation.release(&permit.claim);
        let publication = Publication { completion: CompletionKey { id: permit.id,
                                                                    key } };
        Ok((publication, RetiredPublication { permit }))
    }
}

impl<T, G: InvocationGate, S> PublicationPlan<T, G, S>
{
    /// Run outside every Store/owner/heap guard under the actual owner's Kernel
    /// selection. Only shared directories/index/control use System selection.
    pub fn prepare(self) -> Result<PreparedPublication<T, G, S>, PoolError>
    {
        let error = PoolError::Storage { owner:           self.owner.owner(),
                                         kind:            self.kind,
                                         used:            0,
                                         requested_slots: self.values_slots,
                                         limit:           self.policy.limit() };
        let attach = self.claim.is_none();
        let usage = self.registration.as_ref().map(|kind| kind.acquire(true)).transpose()?;
        let storage_usage = if attach
        {
            self.storage_registration.as_ref().map(|kind| kind.acquire(true)).transpose()?
        }
        else
        {
            None
        };
        let mut owners = Vec::new();
        G::with_system_allocation(||owners.try_reserve_exact(self.owners_capacity)).map_err(|_|error)?;
        let mut ids = HashMap::new();
        G::with_system_allocation(|| ids.try_reserve(self.ids_capacity)).map_err(|_| error)?;
        let mut values = Vec::new();
        values.try_reserve_exact(self.values_slots).map_err(|_| error)?;
        values.resize_with(self.values_slots, || None);
        let state = if attach { Some((self.make_state)(self.owner.owner())?) } else { None };
        let claim = match self.claim
        {
            Some(claim) => claim,
            None =>
            {
                Arc::try_new(Claim { owner:     self.owner.clone(),
                                     kind:      self.kind,
                                     active:    false,
                                     preparing: AtomicUsize::new(0),
                                     retiring:  AtomicBool::new(false) }).map_err(|_| error)?
            }
        };
        let incarnation = self.incarnation;
        let ledger = self.ledger.prepare()?;
        let id = issue_native_identity().map_err(|_| PoolError::IdentityExhausted)?.get();
        let reserve = Reserve::try_new().map_err(|_| error)?;
        let storage_reserve = if attach && self.storage_registration.is_some()
        {
            Some(Reserve::try_new().map_err(|_| error)?)
        }
        else
        {
            None
        };
        Ok(PreparedPublication { owner: self.owner,
                                 kind: self.kind,
                                 registration: self.registration,
                                 storage_registration: self.storage_registration,
                                 usage,
                                 storage_usage,
                                 claim: Some(claim),
                                 attach,
                                 incarnation,
                                 owners,
                                 ids,
                                 values,
                                 state,
                                 ledger,
                                 id,
                                 reserve: Some(reserve),
                                 storage_reserve })
    }
}
