//! Lifetime-domain identity, the resident process owner directory, and ledger
//! metadata. Other managers migrate under memory/app/resource spec §13.
//! Native/legacy wire projections remain the responsibility of the facility.
use alloc::{sync::Arc, vec::Vec};
use core::{fmt,
           num::NonZeroU64,
           sync::atomic::{AtomicBool, AtomicU64, Ordering}};

use crate::shared::{InvocationGate, OwnerRef};

static SYSTEM_INDEX_ISSUED: AtomicBool = AtomicBool::new(false);
// Native identities and domain incarnations share one boot-wide issuer. The
// lower 32-bit space belongs exclusively to the remaining classic projections.
static NEXT_INCARNATION: AtomicU64 = AtomicU64::new(1 << 32);
pub(crate) fn issue_native_identity() -> Result<NonZeroU64, IncarnationsExhausted>
{
    issue(&NEXT_INCARNATION)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomainKind
{
    System,
    Process,
    Module
}
/// Identity is observable, not authority: service admission authenticates callers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DomainId
{
    kind:        DomainKind,
    incarnation: u64
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IncarnationsExhausted;
impl DomainId
{
    pub const SYSTEM: Self = Self { kind:        DomainKind::System,
                                    incarnation: 0 };

    /// The one boot-wide issuer lives in the shared handle facility. It never
    /// resets when a registry/table/domain is destroyed or recreated.
    pub fn issue(kind: DomainKind) -> Result<Self, IncarnationsExhausted>
    {
        if kind == DomainKind::System
        {
            return Ok(Self::SYSTEM);
        }
        let incarnation = issue(&NEXT_INCARNATION)?;
        Ok(Self { kind,
                  incarnation: incarnation.get() })
    }

    pub const fn kind(self) -> DomainKind
    {
        self.kind
    }

    pub const fn incarnation(self) -> u64
    {
        self.incarnation
    }
}
fn issue(counter: &AtomicU64) -> Result<NonZeroU64, IncarnationsExhausted>
{
    counter.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
               (n != 0).then(|| n.checked_add(1).unwrap_or(0))
           })
           .ok()
           .and_then(NonZeroU64::new)
           .ok_or(IncarnationsExhausted)
}
impl fmt::Display for DomainId
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self.kind
        {
            DomainKind::System => f.write_str("System"),
            DomainKind::Process => write!(f, "process incarnation {}", self.incarnation),
            DomainKind::Module => write!(f, "module incarnation {}", self.incarnation)
        }
    }
}
impl fmt::Display for IncarnationsExhausted
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        f.write_str("boot-wide 64-bit resident identity exhausted; an identity cannot be reused")
    }
}
/// A semantic kind code, not an object identity or an allocation-capacity limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KindCode(pub [u8; 4]);
impl fmt::Display for KindCode
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        for byte in self.0
        {
            if byte.is_ascii_graphic()
            {
                write!(f, "{}", byte as char)?;
            }
            else
            {
                write!(f, "\\x{byte:02x}")?;
            }
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapacityPolicy
{
    GrowableUntilMemory,
    PerDomainQuota
    {
        slots: usize
    },
    HardBound
    {
        slots:  usize,
        reason: &'static str
    }
}
impl CapacityPolicy
{
    pub const fn limit(self) -> Option<usize>
    {
        match self
        {
            Self::GrowableUntilMemory => None,
            Self::PerDomainQuota { slots }
            | Self::HardBound { slots,
                              .. } => Some(slots)
        }
    }
}
/// Internal key shape, deliberately not a chosen native wire representation.
/// Including the owner incarnation prevents reuse in a new domain from aliasing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceKey
{
    owner:        DomainId,
    kind:         KindCode,
    slot:         usize,
    generation:   NonZeroU64,
    registration: Option<u64>
}
impl ResourceKey
{
    pub const fn owner(self) -> DomainId
    {
        self.owner
    }

    pub const fn kind(self) -> KindCode
    {
        self.kind
    }

    pub const fn slot(self) -> usize
    {
        self.slot
    }

    pub const fn generation(self) -> NonZeroU64
    {
        self.generation
    }

    /// Boot-unique native identity of the registered release provider. Bootstrap
    /// metadata has no parent registration; observing this value grants no authority.
    pub const fn registration(self) -> Option<u64>
    {
        self.registration
    }
}
/// The host resolves this owned registration capability, never an unpinned
/// function/vtable address supplied by a package.
#[derive(Clone, Copy, Debug)]
pub struct ReleaseDispatch
{
    pub registration: ResourceKey
}
#[derive(Clone, Copy, Debug)]
pub struct KindRegistration
{
    pub code:     KindCode,
    pub capacity: CapacityPolicy,
    pub release:  ReleaseDispatch
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolError
{
    RegistrationUnavailable
    {
        kind:         KindCode,
        registration: ResourceKey
    },
    RegistrationUsesExhausted
    {
        kind:         KindCode,
        registration: ResourceKey
    },
    Scope
    {
        owner: DomainId,
        kind:  KindCode,
        cause: crate::scope::Failure
    },
    InvalidBoundReason,
    ValueStorageAttached
    {
        owner: DomainId,
        kind:  KindCode
    },
    IdentityExhausted,
    UnregisteredKind
    {
        owner: DomainId,
        kind:  KindCode
    },
    SystemIndexAlreadyIssued,
    DuplicateKind
    {
        owner: DomainId,
        kind:  KindCode
    },
    GrowthRequired
    {
        owner:         DomainId,
        kind:          KindCode,
        used:          usize,
        retired:       usize,
        pending:       usize,
        storage_slots: usize,
        policy_limit:  Option<usize>
    },
    InvalidGrowth,
    GrowthLimit
    {
        owner:           DomainId,
        kind:            KindCode,
        requested_slots: usize,
        limit:           usize
    },
    ForeignOwner
    {
        expected: DomainId,
        actual:   DomainId
    },
    Stale
    {
        key: ResourceKey
    },
    Exhausted
    {
        owner:   DomainId,
        kind:    KindCode,
        used:    usize,
        retired: usize,
        pending: usize,
        limit:   usize
    },
    Storage
    {
        owner:           DomainId,
        kind:            KindCode,
        used:            usize,
        requested_slots: usize,
        limit:           Option<usize>
    },
    Stopping
    {
        owner: DomainId
    }
}
impl core::error::Error for PoolError
{
}
impl fmt::Display for PoolError
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Scope { owner,
                          kind,
                          cause } => write!(f, "cannot admit '{kind}' child in {owner}: {cause}"),
            Self::RegistrationUnavailable { kind,
                                            registration } =>
            {
                write!(f,
                       "'{kind}' registration in {} is stopping or stale; no new resources may be \
                        admitted",
                       registration.owner())
            }
            Self::RegistrationUsesExhausted { kind,
                                              registration } =>
            {
                write!(f,
                       "'{kind}' registration in {} exhausted its checked resource/invocation \
                        count",
                       registration.owner())
            }
            Self::ValueStorageAttached { owner,
                                         kind } =>
            {
                write!(f,
                       "'{kind}' in {owner} has exclusive value storage; use its registered store")
            }
            Self::IdentityExhausted => IncarnationsExhausted.fmt(f),
            Self::UnregisteredKind { owner,
                                     kind } =>
            {
                write!(f, "'{kind}' has no registered pool in {owner}")
            }
            Self::SystemIndexAlreadyIssued =>
            {
                f.write_str("System owner index has already been issued this boot; recreation \
                             would lose generation history")
            }
            Self::DuplicateKind { owner,
                                  kind } =>
            {
                write!(f,
                       "'{kind}' pool already exists for {owner}; resetting it would lose \
                        generation history")
            }
            Self::GrowthRequired { owner,
                                   kind,
                                   used,
                                   retired,
                                   pending,
                                   storage_slots,
                                   policy_limit } =>
            {
                write!(f,
                       "'{kind}' metadata growth required for {owner}: {used} live, {retired} \
                        retired, {pending} pending cleanup, {storage_slots} prepared slots, \
                        policy limit {policy_limit:?}")
            }
            Self::InvalidBoundReason =>
            {
                f.write_str("hard handle bound requires a recorded rationale")
            }
            Self::GrowthLimit { owner,
                                kind,
                                requested_slots,
                                limit } =>
            {
                write!(f,
                       "'{kind}' metadata growth refused for {owner}: {requested_slots} requested \
                        slots exceed the {limit} slot policy limit")
            }
            Self::InvalidGrowth =>
            {
                f.write_str("handle metadata growth must add slots without overflow")
            }
            Self::ForeignOwner { expected,
                                 actual } =>
            {
                write!(f, "foreign handle owner: expected {expected}, received {actual}")
            }
            Self::Stale { key } =>
            {
                write!(f,
                       "stale or foreign '{}' handle in {}: slot {}, generation {}",
                       key.kind, key.owner, key.slot, key.generation)
            }
            Self::Exhausted { owner,
                              kind,
                              used,
                              retired,
                              pending,
                              limit } =>
            {
                write!(f,
                       "'{kind}' handle pool exhausted in {owner}: {used} live, {retired} \
                        retired, {pending} pending cleanup, {limit} slot limit")
            }
            Self::Storage { owner,
                            kind,
                            used,
                            requested_slots,
                            limit } =>
            {
                write!(f,
                       "cannot allocate '{kind}' handle metadata for {owner}: {used} live, \
                        {requested_slots} requested slots, policy limit {limit:?}; backing \
                        allocation refused")
            }
            Self::Stopping { owner } => write!(f, "{owner} is stopping; new handles are refused")
        }
    }
}
/// Pure metadata engine for §13.1: out-of-value ownership, occupancy/generation/
/// retirement arrays and rotation. It does not install a second SDK ID scheme.
/// Host synchronization and account charging surround it during migration.
pub struct DomainSlots
{
    owner:                 DomainId,
    kind:                  KindCode,
    policy:                CapacityPolicy,
    occupied:              Vec<u64>,
    retired:               Vec<u64>,
    pending:               Vec<u64>,
    quarantined:           Vec<u64>,
    generations:           Vec<u64>,
    parents:               Vec<Option<ResourceKey>>,
    scoped_count:          usize,
    cursor:                usize,
    live:                  usize,
    retired_count:         usize,
    pending_count:         usize,
    quarantined_count:     usize,
    quarantine_cursor:     usize,
    births:                u64,
    deaths:                u64,
    generation_high_water: u64,
    accepting:             bool,
    drain_word:            usize,
    value_storage:         bool,
    registration:          Option<u64>
}
impl DomainSlots
{
    fn new(owner: DomainId, kind: KindCode, policy: CapacityPolicy) -> Result<Self, PoolError>
    {
        if let CapacityPolicy::HardBound { reason,
                                           .. } = policy
        {
            if reason.trim().is_empty()
            {
                return Err(PoolError::InvalidBoundReason);
            }
        }
        Ok(Self { owner,
                  kind,
                  policy,
                  occupied: Vec::new(),
                  retired: Vec::new(),
                  pending: Vec::new(),
                  quarantined: Vec::new(),
                  generations: Vec::new(),
                  parents: Vec::new(),
                  scoped_count: 0,
                  cursor: 0,
                  live: 0,
                  retired_count: 0,
                  pending_count: 0,
                  quarantined_count: 0,
                  quarantine_cursor: 0,
                  births: 0,
                  deaths: 0,
                  generation_high_water: 0,
                  accepting: true,
                  drain_word: 0,
                  value_storage: false,
                  registration: None })
    }

    /// Capacity comes from the caller's reviewed policy; there is no hidden
    /// initial size or growth increment. All fallible work precedes publication.
    pub fn grow_to(&mut self, slots: usize) -> Result<(), PoolError>
    {
        if !self.accepting
        {
            return Err(PoolError::Stopping { owner: self.owner });
        }
        self.grow_storage(slots)
    }

    fn grow_storage(&mut self, slots: usize) -> Result<(), PoolError>
    {
        if slots <= self.generations.len()
        {
            return Err(PoolError::InvalidGrowth);
        }
        if let Some(limit) = self.policy.limit().filter(|&limit| slots > limit)
        {
            return Err(PoolError::GrowthLimit { owner: self.owner,
                                                kind: self.kind,
                                                requested_slots: slots,
                                                limit });
        }
        let words = slots.div_ceil(u64::BITS as usize);
        let error = PoolError::Storage { owner:           self.owner,
                                         kind:            self.kind,
                                         used:            self.live,
                                         requested_slots: slots,
                                         limit:           self.policy.limit() };
        self.occupied.try_reserve(words - self.occupied.len()).map_err(|_| error)?;
        self.retired.try_reserve(words - self.retired.len()).map_err(|_| error)?;
        self.pending.try_reserve(words - self.pending.len()).map_err(|_| error)?;
        self.quarantined.try_reserve(words - self.quarantined.len()).map_err(|_| error)?;
        self.generations.try_reserve(slots - self.generations.len()).map_err(|_| error)?;
        self.parents.try_reserve(slots - self.parents.len()).map_err(|_| error)?;
        self.occupied.resize(words, 0);
        self.retired.resize(words, 0);
        self.pending.resize(words, 0);
        self.quarantined.resize(words, 0);
        self.generations.resize(slots, 0);
        self.parents.resize(slots, None);
        Ok(())
    }

    fn exhausted(&self) -> PoolError
    {
        PoolError::Exhausted { owner:   self.owner,
                               kind:    self.kind,
                               used:    self.live,
                               retired: self.retired_count,
                               pending: self.pending_count,
                               limit:   self.policy.limit().unwrap_or(self.generations.len()) }
    }

    fn check_owner(&self, owner: DomainId) -> Result<(), PoolError>
    {
        if owner == self.owner
        {
            Ok(())
        }
        else
        {
            Err(PoolError::ForeignOwner { expected: self.owner,
                                          actual:   owner })
        }
    }

    pub fn allocate(&mut self, owner: DomainId) -> Result<ResourceKey, PoolError>
    {
        self.allocate_child(owner, None)
    }

    fn allocate_child(&mut self, owner: DomainId, parent: Option<ResourceKey>)
                      -> Result<ResourceKey, PoolError>
    {
        self.check_owner(owner)?;
        if !self.accepting && parent.is_none()
        {
            return Err(PoolError::Stopping { owner });
        }
        let len = self.generations.len();
        // Scan bitmap words, wrapping at the cursor; never scan stored objects.
        for (start, end) in [(self.cursor, len), (0, self.cursor)]
        {
            let mut at = start;
            while at < end
            {
                let word = at / 64;
                let first = at % 64;
                let take = (end - at).min(64 - first);
                let span = if take == 64 { u64::MAX } else { ((1u64 << take) - 1) << first };
                let free = !(self.occupied[word] | self.retired[word] | self.pending[word]) & span;
                if free != 0
                {
                    let slot = word * 64 + free.trailing_zeros() as usize;
                    let generation = NonZeroU64::new(self.generations[slot].max(1)).unwrap();
                    self.generations[slot] = generation.get();
                    self.generation_high_water = self.generation_high_water.max(generation.get());
                    self.occupied[word] |= 1 << (slot % 64);
                    self.parents[slot] = parent;
                    self.scoped_count += usize::from(parent.is_some());
                    self.live += 1;
                    self.births = self.births.saturating_add(1);
                    self.drain_word = self.drain_word.min(word);
                    self.cursor = if slot + 1 == len { 0 } else { slot + 1 };
                    return Ok(ResourceKey { owner,
                                            kind: self.kind,
                                            slot,
                                            generation,
                                            registration: self.registration });
                }
                at += take;
            }
        }
        if self.policy.limit().is_none_or(|limit| len < limit)
        {
            Err(PoolError::GrowthRequired { owner,
                                            kind: self.kind,
                                            used: self.live,
                                            retired: self.retired_count,
                                            pending: self.pending_count,
                                            storage_slots: len,
                                            policy_limit: self.policy.limit() })
        }
        else
        {
            Err(self.exhausted())
        }
    }

    pub fn contains(&self, key: ResourceKey) -> bool
    {
        key.owner == self.owner
        && key.kind == self.kind
        && key.registration == self.registration
        && key.slot < self.generations.len()
        && self.occupied[key.slot / 64] & (1 << (key.slot % 64)) != 0
        && self.generations[key.slot] == key.generation.get()
    }

    /// Invalidates before returning the cleanup token. The dispatcher runs after
    /// dropping the registry lock; it may recursively release children.
    pub fn invalidate(&mut self, key: ResourceKey) -> Result<ReleasedResource, PoolError>
    {
        if !self.contains(key)
        {
            return Err(PoolError::Stale { key });
        }
        let mask = 1 << (key.slot % 64);
        self.occupied[key.slot / 64] &= !mask;
        self.pending[key.slot / 64] |= mask;
        self.pending_count += 1;
        self.live -= 1;
        self.deaths = self.deaths.saturating_add(1);
        match self.generations[key.slot].checked_add(1)
        {
            Some(next) =>
            {
                self.generations[key.slot] = next;
                self.generation_high_water = self.generation_high_water.max(next);
            }
            None =>
            {
                self.retired[key.slot / 64] |= mask;
                self.retired_count += 1;
                self.generation_high_water = u64::MAX;
            }
        }
        Ok(ReleasedResource { key })
    }

    /// Slot reuse follows completed cleanup, not merely ID invalidation. The
    /// non-cloneable ticket names exactly the old generation. Dropping/forgetting
    /// it leaves a visible pending slot; it can never expose an old payload
    /// through a newly issued identity while its cleanup is outstanding.
    pub fn complete_release(&mut self, released: ReleasedResource) -> Result<(), CompletionError>
    {
        let key = released.key;
        if key.owner != self.owner
        {
            return Err(CompletionError { cause: PoolError::ForeignOwner { expected: self.owner,
                                                                          actual:   key.owner },
                                         released });
        }
        if !self.pending_key(key) || self.quarantined[key.slot / 64] & (1 << (key.slot % 64)) != 0
        {
            return Err(CompletionError::stale(released));
        }
        self.pending[key.slot / 64] &= !(1 << (key.slot % 64));
        self.pending_count -= 1;
        self.scoped_count -= usize::from(self.parents[key.slot].take().is_some());
        Ok(())
    }

    fn pending_key(&self, key: ResourceKey) -> bool
    {
        key.owner == self.owner
        && key.kind == self.kind
        && key.registration == self.registration
        && key.slot < self.generations.len()
        && self.pending[key.slot / 64] & (1 << (key.slot % 64)) != 0
        && self.generations[key.slot] == key.generation.get().saturating_add(1)
        && (self.retired[key.slot / 64] & (1 << (key.slot % 64)) != 0)
           == (key.generation.get() == u64::MAX)
    }

    fn quarantine(&mut self, released: ReleasedResource) -> Result<(), CompletionError>
    {
        let key = released.key;
        if !self.pending_key(key) || self.quarantined[key.slot / 64] & (1 << (key.slot % 64)) != 0
        {
            return Err(CompletionError::stale(released));
        }
        // Consuming the unique ticket transfers its ownership to this bitmap.
        // Backing was reserved with the pool; failure reporting cannot allocate.
        self.quarantined[key.slot / 64] |= 1 << (key.slot % 64);
        self.quarantined_count += 1;
        Ok(())
    }

    fn resume_quarantined(&mut self) -> Option<ReleasedResource>
    {
        let len = self.generations.len();
        for (start, end) in [(self.quarantine_cursor, len), (0, self.quarantine_cursor)]
        {
            if let Some(ticket) = self.resume_quarantined_range(start, end)
            {
                self.quarantine_cursor =
                    if ticket.key.slot + 1 == len { 0 } else { ticket.key.slot + 1 };
                return Some(ticket);
            }
        }
        None
    }

    fn resume_quarantined_range(&mut self, start: usize, end: usize) -> Option<ReleasedResource>
    {
        let end = end.min(self.generations.len());
        let mut at = start;
        while at < end
        {
            let word = at / 64;
            let first = at % 64;
            let take = (end - at).min(64 - first);
            let mask = if take == 64 { u64::MAX } else { ((1u64 << take) - 1) << first };
            let bits = self.quarantined[word] & mask;
            if bits != 0
            {
                let slot = word * 64 + bits.trailing_zeros() as usize;
                let generation = if self.retired[word] & (1 << (slot % 64)) != 0
                {
                    u64::MAX
                }
                else
                {
                    self.generations[slot].checked_sub(1)?
                };
                let key = ResourceKey { owner: self.owner,
                                        kind: self.kind,
                                        slot,
                                        generation: NonZeroU64::new(generation)?,
                                        registration: self.registration };
                // Finish structural validation before transferring the unique
                // ticket out of its reserved quarantine bitmap.
                self.quarantined[word] &= !(1 << (slot % 64));
                self.quarantined_count -= 1;
                return Some(ReleasedResource { key });
            }
            at += take;
        }
        None
    }

    pub fn stop_admission(&mut self)
    {
        self.accepting = false;
    }

    pub fn invalidate_next(&mut self) -> Option<ReleasedResource>
    {
        let Some(relative) = self.occupied[self.drain_word..].iter().position(|&word| word != 0)
        else
        {
            self.drain_word = self.occupied.len();
            return None;
        };
        let word = self.drain_word + relative;
        self.drain_word = word;
        let slot = word * 64 + self.occupied[word].trailing_zeros() as usize;
        let key = ResourceKey { owner: self.owner,
                                kind: self.kind,
                                slot,
                                generation: NonZeroU64::new(self.generations[slot])?,
                                registration: self.registration };
        self.invalidate(key).ok()
    }

    pub const fn owner(&self) -> DomainId
    {
        self.owner
    }

    pub const fn kind(&self) -> KindCode
    {
        self.kind
    }

    pub const fn capacity_policy(&self) -> CapacityPolicy
    {
        self.policy
    }

    pub const fn live(&self) -> usize
    {
        self.live
    }

    pub const fn retired(&self) -> usize
    {
        self.retired_count
    }

    pub const fn pending(&self) -> usize
    {
        self.pending_count
    }

    pub fn storage_slots(&self) -> usize
    {
        self.generations.len()
    }

    /// Cumulative successful publications and invalidations. Counters saturate
    /// rather than wrap; inspectors must report saturation instead of a rate.
    pub fn counters(&self) -> (u64, u64)
    {
        (self.births, self.deaths)
    }
}
/// Not Clone/Copy: one invalidation produces one dispatch ticket.
#[must_use = "dispatch cleanup, then complete_release; an unfinished ticket keeps its slot pending"]
#[derive(Debug)]
pub struct ReleasedResource
{
    key: ResourceKey
}

/// Refusal retains the cleanup capability so the host can retry against its
/// proper owner. An erroneous completion never silently strands a live ticket.
#[derive(Debug)]
pub struct CompletionError
{
    cause:    PoolError,
    released: ReleasedResource
}
impl CompletionError
{
    pub(crate) fn stale(released: ReleasedResource) -> Self
    {
        Self { cause: PoolError::Stale { key: released.key },
               released }
    }

    pub fn cause(&self) -> PoolError
    {
        self.cause
    }

    pub fn into_ticket(self) -> ReleasedResource
    {
        self.released
    }
}
impl fmt::Display for CompletionError
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        write!(f, "cleanup completion refused: {}; the ticket remains owned for retry", self.cause)
    }
}
impl core::error::Error for CompletionError
{
}
impl ReleasedResource
{
    pub const fn key(&self) -> ResourceKey
    {
        self.key
    }
}
/// A bounded metadata scan, not resource authority. Only its originating ledger
/// can transfer tickets through it. No callback receives the private cursors.
pub(crate) struct QuarantineRetry
{
    owner: DomainId,
    pools: usize,
    pool:  usize,
    slot:  usize,
    limit: Option<usize>
}

/// Central owner index shape; registering pools/dispatchers and draining queues
/// are the next runtime migration. No manager-specific cleanup list belongs here.
pub struct OwnerIndex
{
    owner:                  DomainId,
    pools:                  Vec<DomainSlots>,
    admission:              Arc<AtomicBool>,
    revision:               u64,
    quarantine_pool_cursor: usize,
    completions:            crate::completion::Queue
}

/// One locked read of the authoritative ledger; no object-value scan or allocation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LedgerSummary
{
    pub revision:          u64,
    pub kinds:             usize,
    pub live:              usize,
    pub retired:           usize,
    pub pending:           usize,
    pub quarantined:       usize,
    pub completion_faults: usize,
    pub scoped_children:   usize,
    pub slots:             usize,
    pub births:            u64,
    pub deaths:            u64,
    pub accepting:         bool
}

/// An atomic view of one prepared per-domain pool, not a global kind
/// registration. No payload or generation-array scan is required.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolSummary
{
    pub registration:        Option<u64>,
    pub kind:                KindCode,
    pub policy:              CapacityPolicy,
    pub live:                usize,
    pub retired:             usize,
    pub pending:             usize,
    pub quarantined:         usize,
    pub completion_faults:   usize,
    pub scoped_children:     usize,
    pub slots:               usize,
    pub births:              u64,
    pub deaths:              u64,
    /// Remaining increments of the most advanced slot, including retired
    /// history. Zero does not imply all other slots are exhausted.
    pub generation_headroom: u64
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LedgerReadError
{
    Stale
    {
        owner: DomainId, expected: u64, actual: u64
    },
    RevisionExhausted
    {
        owner: DomainId
    },
    PoolNotFound
    {
        owner: DomainId, index: usize
    }
}
impl fmt::Display for LedgerReadError
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Stale { owner,
                          expected,
                          actual } =>
            {
                write!(f,
                       "{owner} ledger changed from revision {expected} to {actual}; obtain a new \
                        ledger snapshot and retry")
            }
            Self::RevisionExhausted { owner } =>
            {
                write!(f,
                       "{owner} ledger revision exhausted; restart this domain to obtain new \
                        snapshots")
            }
            Self::PoolNotFound { owner,
                                 index } =>
            {
                write!(f, "pool index {index} is outside {owner}'s ledger snapshot")
            }
        }
    }
}
impl core::error::Error for LedgerReadError
{
}

/// Shared owner directory. The ledger and payload cannot acquire different
/// identities: the directory derives its key exclusively from the owned index.
/// Entries stay ordered by boot incarnation for logarithmic identity lookup.
/// Removing a domain moves only directory metadata, never its resident objects.
pub struct Directory<T, G: InvocationGate>
{
    entries:  Vec<DomainEntry<T, G>>,
    revision: u64
}

pub struct DomainEntry<T, G: InvocationGate>
{
    ledger: OwnerRef<G>,
    value:  T
}
impl<T, G: InvocationGate> DomainEntry<T, G>
{
    pub fn owner(&self) -> DomainId
    {
        self.ledger.owner()
    }

    pub const fn ledger(&self) -> &OwnerRef<G>
    {
        &self.ledger
    }

    pub const fn value(&self) -> &T
    {
        &self.value
    }

    pub fn value_mut(&mut self) -> &mut T
    {
        &mut self.value
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryError
{
    Storage
    {
        owner: DomainId
    },
    NotFound
    {
        incarnation: u64
    },
    LiveResources
    {
        owner: DomainId, live: usize
    },
    PendingResources
    {
        owner: DomainId, pending: usize
    },
    CompletionFaults
    {
        owner: DomainId, faults: usize
    },
    AdmissionOpen
    {
        owner: DomainId
    }
}
impl fmt::Display for DirectoryError
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Storage { owner } => write!(f, "owner-directory allocation refused for {owner}"),
            Self::AdmissionOpen { owner } =>
            {
                write!(f, "cannot drain {owner}: resource admission is still open")
            }
            Self::NotFound { incarnation } =>
            {
                write!(f, "lifetime domain incarnation {incarnation} not found")
            }
            Self::LiveResources { owner,
                                  live } =>
            {
                write!(f, "cannot retire {owner}: {live} live ledger resources remain")
            }
            Self::CompletionFaults { owner,
                                     faults } =>
            {
                write!(f,
                       "{owner} retains {faults} cleanup completion faults; tickets and provider \
                        pins require recovery before domain retirement")
            }
            Self::PendingResources { owner,
                                     pending } =>
            {
                write!(f,
                       "cannot retire {owner}: cleanup of {pending} invalidated ledger resources \
                        is still pending")
            }
        }
    }
}
impl<T, G: InvocationGate> Directory<T, G>
{
    pub const fn new() -> Self
    {
        Self { entries:  Vec::new(),
               revision: 1 }
    }

    pub fn len(&self) -> usize
    {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool
    {
        self.entries.is_empty()
    }

    /// Consumes the sole owner index. Neither insertion nor payload mutation
    /// allows the caller to substitute another owner's ledger afterward.
    pub fn insert(&mut self, ledger: OwnerIndex, value: T) -> Result<DomainId, DirectoryError>
    {
        let owner = ledger.owner();
        let ledger = OwnerRef::try_new(ledger)?;
        // OwnerIndex's issuer prohibits duplicate incarnations, including System.
        let at =
            self.entries.partition_point(|entry| entry.owner().incarnation() < owner.incarnation());
        self.entries.try_reserve(1).map_err(|_| DirectoryError::Storage { owner })?;
        self.entries.insert(at,
                            DomainEntry { ledger,
                                          value });
        self.revision = self.revision.saturating_add(1);
        Ok(owner)
    }

    fn index(&self, incarnation: u64) -> Option<usize>
    {
        self.entries.binary_search_by_key(&incarnation, |entry| entry.owner().incarnation()).ok()
    }

    /// Native process IDs are the complete boot-unique incarnation, not a
    /// truncated slot/generation projection. Managers still enforce caller policy.
    pub fn get(&self, incarnation: u64) -> Option<&DomainEntry<T, G>>
    {
        self.entries.get(self.index(incarnation)?)
    }

    /// Mutable access is restricted to the payload: exposing &mut DomainEntry
    /// would let callers swap owner indices and break ordering/identity.
    pub fn get_mut(&mut self, incarnation: u64) -> Option<&mut T>
    {
        let at = self.index(incarnation)?;
        self.entries.get_mut(at).map(DomainEntry::value_mut)
    }

    pub fn iter(&self) -> impl Iterator<Item = &DomainEntry<T, G>>
    {
        self.entries.iter()
    }

    /// Membership epoch. Saturation invalidates inspection snapshots permanently;
    /// it must never make a stale snapshot current again.
    pub fn revision(&self) -> u64
    {
        self.revision
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut T>
    {
        self.entries.iter_mut().map(DomainEntry::value_mut)
    }

    /// Borrow the authentic ledger beside its mutable payload. The entry and
    /// owner index remain immutable, so callers cannot replace identities or
    /// break the directory's incarnation ordering while applying owner policy.
    pub fn entries_mut(&mut self) -> impl Iterator<Item = (&OwnerRef<G>, &mut T)>
    {
        self.entries.iter_mut().map(|entry| (&entry.ledger, &mut entry.value))
    }

    pub fn stop_admission(&mut self, incarnation: u64) -> Result<(), DirectoryError>
    {
        let at = self.index(incarnation).ok_or(DirectoryError::NotFound { incarnation })?;
        self.entries[at].ledger.stop_admission();
        Ok(())
    }

    /// The invalidation ticket is dispatched after releasing the directory's
    /// lock; cleanup may recursively invalidate other resources.
    pub fn invalidate_next(&mut self, incarnation: u64)
                           -> Result<Option<ReleasedResource>, DirectoryError>
    {
        let at = self.index(incarnation).ok_or(DirectoryError::NotFound { incarnation })?;
        Ok(self.entries[at].ledger.invalidate_next())
    }

    /// Retirement is legal only after the common walk has emptied the ledger.
    /// History disappears with this domain; its incarnation can never recur.
    pub fn remove(&mut self, incarnation: u64) -> Result<T, DirectoryError>
    {
        let at = self.index(incarnation).ok_or(DirectoryError::NotFound { incarnation })?;
        self.entries[at].ledger.retire()?;
        self.revision = self.revision.saturating_add(1);
        Ok(self.entries.remove(at).value)
    }
}
impl<T, G: InvocationGate> Default for Directory<T, G>
{
    fn default() -> Self
    {
        Self::new()
    }
}
impl<T, G: InvocationGate> Drop for Directory<T, G>
{
    fn drop(&mut self)
    {
        // Stop orphan references issuing new objects. Existing keys remain for
        // RAII cleanup; kind dispatch/draining still belongs to the explicit walk.
        for entry in &self.entries
        {
            entry.ledger.stop_admission();
        }
    }
}

#[derive(Debug)]
pub enum IndexError
{
    Identity(IncarnationsExhausted),
    AdmissionStorage
    {
        kind: DomainKind
    },
    SharedStorage
    {
        kind: DomainKind
    },
    Pool(PoolError)
}
impl fmt::Display for IndexError
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Identity(e) => e.fmt(f),
            Self::AdmissionStorage { kind } =>
            {
                write!(f,
                       "cannot allocate {kind:?} owner admission authority; backing allocation \
                        refused")
            }
            Self::SharedStorage { kind } =>
            {
                write!(f,
                       "cannot allocate {kind:?} shared owner authority; backing allocation \
                        refused")
            }
            Self::Pool(e) => e.fmt(f)
        }
    }
}

/// A fresh, empty namespace that has not escaped as owner authority. Its only
/// publication paths commit the boot-wide issuer before returning the index or
/// shared root. No fallible backing allocation follows that commitment.
pub(crate) struct UnpublishedIndex
{
    index: OwnerIndex
}
impl UnpublishedIndex
{
    pub(crate) fn prepare(kind: DomainKind) -> Result<Self, IndexError>
    {
        if kind == DomainKind::System && SYSTEM_INDEX_ISSUED.load(Ordering::Relaxed)
        {
            return Err(IndexError::Pool(PoolError::SystemIndexAlreadyIssued));
        }
        let admission =
            Arc::try_new(AtomicBool::new(true)).map_err(|_| IndexError::AdmissionStorage { kind })?;
        let owner = DomainId::issue(kind).map_err(IndexError::Identity)?;
        Ok(Self { index: OwnerIndex { owner,
                                      pools: Vec::new(),
                                      admission,
                                      revision: 1,
                                      quarantine_pool_cursor: 0,
                                      completions: crate::completion::Queue::new() } })
    }

    fn commit(kind: DomainKind) -> Result<(), IndexError>
    {
        if kind == DomainKind::System
           && SYSTEM_INDEX_ISSUED.compare_exchange(false,
                                                   true,
                                                   Ordering::Relaxed,
                                                   Ordering::Relaxed)
                                 .is_err()
        {
            return Err(IndexError::Pool(PoolError::SystemIndexAlreadyIssued));
        }
        Ok(())
    }

    fn into_standalone(self) -> Result<OwnerIndex, IndexError>
    {
        Self::commit(self.index.owner.kind())?;
        Ok(self.index)
    }

    pub(crate) fn into_shared<G: InvocationGate>(self) -> Result<OwnerRef<G>, IndexError>
    {
        let kind = self.index.owner.kind();
        // Neither the candidate nor a clone escapes before both Arc allocations
        // succeed. Refusal drops only unpublished backing, leaving System free
        // for a genuine retry; failed non-System incarnations remain consumed.
        let root = OwnerRef::try_new(self.index).map_err(|_| IndexError::SharedStorage { kind })?;
        G::run(|| Self::commit(kind))?;
        Ok(root)
    }
}

impl OwnerIndex
{
    pub fn quarantine(&mut self, released: ReleasedResource) -> Result<(), CompletionError>
    {
        if released.key.owner != self.owner
        {
            return Err(CompletionError { cause: PoolError::ForeignOwner { expected: self.owner,
                                                                          actual:   released.key
                                                                                            .owner },
                                         released });
        }
        let Some(pool) = self.pool_mut(released.key.kind)
        else
        {
            return Err(CompletionError::stale(released));
        };
        pool.quarantine(released)?;
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    pub fn resume_quarantined(&mut self) -> Option<ReleasedResource>
    {
        let count = self.pools.len();
        for offset in 0..count
        {
            let at = (self.quarantine_pool_cursor + offset) % count;
            if let Some(ticket) = self.pools[at].resume_quarantined()
            {
                self.quarantine_pool_cursor = (at + 1) % count;
                self.revision = self.revision.saturating_add(1);
                return Some(ticket);
            }
        }
        None
    }

    pub(crate) fn quarantine_retry(&self) -> QuarantineRetry
    {
        QuarantineRetry { owner: self.owner,
                          pools: self.pools.len(),
                          pool:  0,
                          slot:  0,
                          limit: None }
    }

    pub(crate) fn resume_quarantined_in(&mut self, cursor: &mut QuarantineRetry)
                                        -> Option<ReleasedResource>
    {
        if cursor.owner != self.owner
        {
            return None;
        }
        while cursor.pool < cursor.pools
        {
            let pool = self.pools.get_mut(cursor.pool)?;
            let limit = *cursor.limit.get_or_insert(pool.generations.len());
            if let Some(ticket) = pool.resume_quarantined_range(cursor.slot, limit)
            {
                cursor.slot = ticket.key.slot + 1;
                self.revision = self.revision.saturating_add(1);
                return Some(ticket);
            }
            cursor.pool += 1;
            cursor.slot = 0;
            cursor.limit = None;
        }
        None
    }

    pub fn pool_at(&self, revision: u64, index: usize) -> Result<PoolSummary, LedgerReadError>
    {
        if self.revision == u64::MAX
        {
            return Err(LedgerReadError::RevisionExhausted { owner: self.owner });
        }
        if revision != self.revision
        {
            return Err(LedgerReadError::Stale { owner:    self.owner,
                                                expected: revision,
                                                actual:   self.revision });
        }
        let pool =
            self.pools.get(index).ok_or(LedgerReadError::PoolNotFound { owner: self.owner,
                                                                         index })?;
        Ok(PoolSummary { registration:        pool.registration,
                         kind:                pool.kind,
                         policy:              pool.policy,
                         live:                pool.live,
                         retired:             pool.retired_count,
                         pending:             pool.pending_count,
                         quarantined:         pool.quarantined_count,
                         completion_faults:   self.completions.count_kind(pool.kind),
                         scoped_children:     pool.scoped_count,
                         slots:               pool.generations.len(),
                         births:              pool.births,
                         deaths:              pool.deaths,
                         generation_headroom: u64::MAX - pool.generation_high_water })
    }

    pub fn summary(&self) -> LedgerSummary
    {
        let mut result = LedgerSummary { revision: self.revision,
                                         kinds: self.pools.len(),
                                         completion_faults: self.completions.len(),
                                         accepting: self.accepting(),
                                         ..Default::default() };
        for pool in &self.pools
        {
            result.live += pool.live();
            result.retired += pool.retired();
            result.pending += pool.pending();
            result.scoped_children += pool.scoped_count;
            result.quarantined += pool.quarantined_count;
            result.slots += pool.storage_slots();
            let (births, deaths) = pool.counters();
            result.births = result.births.saturating_add(births);
            result.deaths = result.deaths.saturating_add(deaths);
        }
        result
    }

    /// Only this factory can create a domain's pools. Dropping an index never
    /// authorizes recreating its incarnation or resetting its generations.
    pub fn issue(kind: DomainKind) -> Result<Self, IndexError>
    {
        UnpublishedIndex::prepare(kind)?.into_standalone()
    }

    pub(crate) fn note_scope_change(&mut self)
    {
        self.revision = self.revision.saturating_add(1);
    }

    pub(crate) fn retain_completion(&mut self, record: alloc::boxed::Box<crate::completion::Record>)
    {
        self.completions.push(record);
        self.revision = self.revision.saturating_add(1);
    }

    pub(crate) fn retry_completion(&mut self) -> crate::completion::Retry
    {
        let mut queue = core::mem::replace(&mut self.completions, crate::completion::Queue::new());
        let previous = queue.next_fault();
        let result = queue.retry_one(self);
        self.completions = queue;
        if matches!(result, crate::completion::Retry::Complete(_))
           || matches!(&result, crate::completion::Retry::Retained(fault) if Some(*fault)!=previous)
        {
            self.revision = self.revision.saturating_add(1);
        }
        result
    }

    pub(crate) fn route_completion(&mut self, ticket: ReleasedResource)
                                   -> crate::completion::Routed
    {
        let mut queue = core::mem::replace(&mut self.completions, crate::completion::Queue::new());
        let result = queue.route(self, ticket);
        self.completions = queue;
        if !matches!(result, crate::completion::Routed::Continue(_))
        {
            self.revision = self.revision.saturating_add(1);
        }
        result
    }

    pub fn completion_fault_at(&self, revision: u64, at: usize)
                               -> Result<Option<crate::completion::Fault>, LedgerReadError>
    {
        if self.revision == u64::MAX
        {
            return Err(LedgerReadError::RevisionExhausted { owner: self.owner });
        }
        if revision != self.revision
        {
            return Err(LedgerReadError::Stale { owner:    self.owner,
                                                expected: revision,
                                                actual:   self.revision });
        }
        Ok(self.completions.at(at))
    }

    pub(crate) fn authorize(&self, kind: KindCode, scope: Option<&crate::scope::State>)
                            -> Result<Option<ResourceKey>, PoolError>
    {
        if let Some(scope) = scope
        {
            return scope.authorize(self).map(Some).map_err(|cause| {
                                                      PoolError::Scope { owner: self.owner,
                                                                         kind,
                                                                         cause }
                                                  });
        }
        if !self.accepting()
        {
            return Err(PoolError::Stopping { owner: self.owner });
        }
        Ok(None)
    }

    pub(crate) fn attach_values_with(&mut self, kind: KindCode, policy: CapacityPolicy,
                                     registration: Option<u64>,
                                     scope: Option<&crate::scope::State>)
                                     -> Result<usize, PoolError>
    {
        self.authorize(kind, scope)?;
        if !self.pools.iter().any(|pool| pool.kind == kind)
        {
            self.add_authorized_pool(kind, policy)?;
        }
        let owner = self.owner;
        let pool = self.pool_mut(kind).unwrap();
        if pool.value_storage || pool.live != 0 || pool.pending_count != 0 || pool.policy != policy
        {
            return Err(PoolError::ValueStorageAttached { owner,
                                                         kind });
        }
        pool.value_storage = true;
        let slots = pool.storage_slots();
        if pool.registration != registration
        {
            pool.registration = registration;
            self.revision = self.revision.saturating_add(1);
        }
        Ok(slots)
    }

    pub(crate) fn detach_values(&mut self, kind: KindCode)
    {
        if let Some(pool) = self.pool_mut(kind)
        {
            pool.value_storage = false;
        }
    }

    pub const fn owner(&self) -> DomainId
    {
        self.owner
    }

    pub fn accepting(&self) -> bool
    {
        self.admission.load(Ordering::Acquire)
    }

    /// The shared reference observes this same authority without entering the
    /// ledger lock, including allocator admission during metadata growth.
    pub(crate) fn admission(&self) -> Arc<AtomicBool>
    {
        self.admission.clone()
    }

    pub fn pools(&self) -> impl Iterator<Item = &DomainSlots>
    {
        self.pools.iter()
    }

    fn pool_mut(&mut self, kind: KindCode) -> Option<&mut DomainSlots>
    {
        self.pools.iter_mut().find(|p| p.kind == kind)
    }

    pub fn add_pool(&mut self, kind: KindCode, policy: CapacityPolicy) -> Result<(), PoolError>
    {
        if !self.accepting()
        {
            return Err(PoolError::Stopping { owner: self.owner });
        }
        self.add_authorized_pool(kind, policy)
    }

    fn add_authorized_pool(&mut self, kind: KindCode, policy: CapacityPolicy)
                           -> Result<(), PoolError>
    {
        if self.pools.iter().any(|p| p.kind == kind)
        {
            return Err(PoolError::DuplicateKind { owner: self.owner,
                                                  kind });
        }
        let mut pool = DomainSlots::new(self.owner, kind, policy)?;
        if !self.accepting()
        {
            pool.stop_admission();
        }
        self.pools.try_reserve(1).map_err(|_| {
                                      PoolError::Storage { owner: self.owner,
                                                           kind,
                                                           used: self.pools.len(),
                                                           requested_slots: 1,
                                                           limit: None }
                                  })?;
        self.pools.push(pool);
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    fn check_metadata_access(&self, kind: KindCode) -> Result<(), PoolError>
    {
        if self.pools.iter().any(|pool| pool.kind == kind && pool.value_storage)
        {
            return Err(PoolError::ValueStorageAttached { owner: self.owner,
                                                         kind });
        }
        Ok(())
    }

    pub fn grow(&mut self, kind: KindCode, slots: usize) -> Result<(), PoolError>
    {
        self.check_metadata_access(kind)?;
        self.grow_values(kind, slots)
    }

    pub(crate) fn grow_values(&mut self, kind: KindCode, slots: usize) -> Result<(), PoolError>
    {
        self.grow_values_with(kind, slots, None)
    }

    pub(crate) fn grow_values_with(&mut self, kind: KindCode, slots: usize,
                                   scope: Option<&crate::scope::State>)
                                   -> Result<(), PoolError>
    {
        let parent = self.authorize(kind, scope)?;
        let owner = self.owner;
        let pool = self.pool_mut(kind).ok_or(PoolError::UnregisteredKind { owner,
                                                                            kind })?;
        if parent.is_some()
        {
            pool.grow_storage(slots)?;
        }
        else
        {
            pool.grow_to(slots)?;
        }
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    pub fn allocate(&mut self, kind: KindCode) -> Result<ResourceKey, PoolError>
    {
        self.check_metadata_access(kind)?;
        self.allocate_value(kind)
    }

    pub(crate) fn allocate_value(&mut self, kind: KindCode) -> Result<ResourceKey, PoolError>
    {
        self.allocate_value_with(kind, None)
    }

    pub(crate) fn allocate_value_with(&mut self, kind: KindCode,
                                      scope: Option<&crate::scope::State>)
                                      -> Result<ResourceKey, PoolError>
    {
        let parent = self.authorize(kind, scope)?;
        let owner = self.owner;
        let key = self.pool_mut(kind)
                      .ok_or(PoolError::UnregisteredKind { owner,
                                                           kind })?
                      .allocate_child(owner, parent)?;
        self.revision = self.revision.saturating_add(1);
        Ok(key)
    }

    /// Full parent identity for a live or pending child; it never validates a
    /// stale child generation or upgrades metadata into scope authority.
    pub fn parent(&self, key: ResourceKey) -> Option<ResourceKey>
    {
        if !self.contains(key) && !self.pending(key)
        {
            return None;
        }
        *self.pools.iter().find(|pool| pool.kind == key.kind())?.parents.get(key.slot())?
    }

    pub fn contains(&self, key: ResourceKey) -> bool
    {
        self.pools.iter().find(|p| p.kind == key.kind()).is_some_and(|p| p.contains(key))
    }

    pub fn pending(&self, key: ResourceKey) -> bool
    {
        self.pools.iter().find(|p| p.kind == key.kind()).is_some_and(|p| p.pending_key(key))
    }

    pub fn invalidate(&mut self, key: ResourceKey) -> Result<ReleasedResource, PoolError>
    {
        let released =
            self.pool_mut(key.kind()).ok_or(PoolError::Stale { key })?.invalidate(key)?;
        self.revision = self.revision.saturating_add(1);
        Ok(released)
    }

    pub fn complete_release(&mut self, released: ReleasedResource) -> Result<(), CompletionError>
    {
        if let Err(cause) = self.check_metadata_access(released.key.kind)
        {
            return Err(CompletionError { cause,
                                         released });
        }
        self.complete_value_release(released)
    }

    pub(crate) fn complete_value_release(&mut self, released: ReleasedResource)
                                         -> Result<(), CompletionError>
    {
        let key = released.key;
        if key.owner != self.owner
        {
            return Err(CompletionError { cause: PoolError::ForeignOwner { expected: self.owner,
                                                                          actual:   key.owner },
                                         released });
        }
        let Some(pool) = self.pool_mut(key.kind())
        else
        {
            return Err(CompletionError::stale(released));
        };
        pool.complete_release(released)?;
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    pub fn stop_admission(&mut self)
    {
        // Publish closure before stopping the individual pools. Shared reads
        // never take this lock; writers remain inside the invocation gate.
        if self.admission.swap(false, Ordering::AcqRel)
        {
            self.revision = self.revision.saturating_add(1);
        }
        for pool in &mut self.pools
        {
            pool.stop_admission();
        }
    }

    fn ensure_empty(&self) -> Result<(), DirectoryError>
    {
        if self.completions.len() != 0
        {
            return Err(DirectoryError::CompletionFaults { owner:  self.owner,
                                                          faults: self.completions.len() });
        }
        let live = self.live();
        if live != 0
        {
            return Err(DirectoryError::LiveResources { owner: self.owner,
                                                       live });
        }
        let pending = self.pools.iter().map(DomainSlots::pending).sum();
        if pending != 0
        {
            return Err(DirectoryError::PendingResources { owner: self.owner,
                                                          pending });
        }
        Ok(())
    }

    /// Detach only a quiescent, closed domain's metadata. The caller must drop
    /// the returned backing after leaving its owner lock. The stopped identity
    /// remains authoritative and can never acquire a replacement namespace.
    pub(crate) fn drain_stopped(&mut self) -> Result<Vec<DomainSlots>, DirectoryError>
    {
        self.ensure_empty()?;
        if self.accepting()
        {
            return Err(DirectoryError::AdmissionOpen { owner: self.owner });
        }
        Ok(self.detach_pools())
    }

    fn detach_pools(&mut self) -> Vec<DomainSlots>
    {
        if !self.pools.is_empty()
        {
            self.revision = self.revision.saturating_add(1);
        }
        self.quarantine_pool_cursor = 0;
        core::mem::take(&mut self.pools)
    }

    pub(crate) fn retire(&mut self) -> Result<Vec<DomainSlots>, DirectoryError>
    {
        self.ensure_empty()?;
        // Existing callers may retire an empty open ledger. Check and close
        // under the same metadata lock before detaching any generation history.
        self.stop_admission();
        Ok(self.detach_pools())
    }

    /// Kind dispatch is resolved by the caller after dropping its registry lock.
    pub fn invalidate_next(&mut self) -> Option<ReleasedResource>
    {
        let released = self.pools.iter_mut().find_map(DomainSlots::invalidate_next)?;
        self.revision = self.revision.saturating_add(1);
        Some(released)
    }

    pub fn live(&self) -> usize
    {
        self.pools.iter().map(DomainSlots::live).sum()
    }
}

#[cfg(test)]
mod tests
{
    use super::*;

    #[test]
    fn quarantine_retry_cursor_cannot_transfer_another_owner_ticket()
    {
        let kind = KindCode(*b"RETR");
        let mut first = OwnerIndex::issue(DomainKind::Module).unwrap();
        let mut second = OwnerIndex::issue(DomainKind::Module).unwrap();
        for owner in [&mut first, &mut second]
        {
            owner.add_pool(kind, CapacityPolicy::GrowableUntilMemory).unwrap();
            owner.grow(kind, 1).unwrap();
            let key = owner.allocate(kind).unwrap();
            let ticket = owner.invalidate(key).unwrap();
            owner.quarantine(ticket).unwrap();
        }
        let mut cursor = first.quarantine_retry();
        let original = second.summary();
        assert!(second.resume_quarantined_in(&mut cursor).is_none());
        assert_eq!(second.summary(), original);
        let own = first.resume_quarantined_in(&mut cursor).unwrap();
        assert_eq!(own.key().owner(), first.owner());
        first.complete_release(own).unwrap();
        assert!(first.resume_quarantined_in(&mut cursor).is_none());
        let retained = second.resume_quarantined().unwrap();
        second.complete_release(retained).unwrap();
    }
    #[test]
    fn retirement_discards_domain_history_without_reopening_its_incarnation()
    {
        let mut index = OwnerIndex::issue(DomainKind::Process).unwrap();
        let owner = index.owner();
        let kind = KindCode(*b"META");
        index.add_pool(kind, CapacityPolicy::GrowableUntilMemory).unwrap();
        index.grow(kind, 1024).unwrap();
        let key = index.allocate(kind).unwrap();
        assert!(matches!(index.retire(),
                         Err(DirectoryError::LiveResources { live: 1,
                                                             .. })));
        assert!(index.accepting());
        let released = index.invalidate(key).unwrap();
        index.complete_release(released).unwrap();
        index.retire().unwrap();
        assert_eq!(index.owner(), owner);
        assert_eq!(index.pools.capacity(),
                   0,
                   "retirement must release the allocation, not only clear occupancy");
        assert!(!index.contains(key));
        assert!(matches!(index.add_pool(kind, CapacityPolicy::GrowableUntilMemory),
                         Err(PoolError::Stopping { .. })));
        assert!(matches!(index.allocate(kind), Err(PoolError::Stopping { .. })));
    }
    struct HostGate;
    impl InvocationGate for HostGate
    {
        fn run<R>(operation: impl FnOnce() -> R) -> R
        {
            operation()
        }
    }
    #[test]
    fn directory_does_not_truncate_wide_incarnations()
    {
        // Private-unit fixture only; no public identity-forging constructor or
        // service trap. Exercise both sides of the u32 boundary without billions
        // of launches or mutating the actual boot-wide issuer.
        let ledger = |incarnation| {
            OwnerIndex { owner:                  DomainId { kind: DomainKind::Process,
                                                            incarnation },
                         pools:                  Vec::new(),
                         admission:              Arc::new(AtomicBool::new(true)),
                         revision:               1,
                         quarantine_pool_cursor: 0,
                         completions:            crate::completion::Queue::new() }
        };
        let low = 7;
        let high = (1u64 << 40) | low;
        let mut directory = Directory::<_, HostGate>::new();
        directory.insert(ledger(low), "low").unwrap();
        directory.insert(ledger(high), "high").unwrap();
        assert_eq!(directory.get(low).unwrap().value(), &"low");
        assert_eq!(directory.get(high).unwrap().value(), &"high");
        assert_eq!(directory.remove(high).unwrap(), "high");
        assert!(directory.get(high).is_none());
        assert_eq!(directory.get(low).unwrap().value(), &"low");
    }
    #[test]
    fn mutable_payload_iteration_retains_authentic_directory_identity()
    {
        let mut directory = Directory::<_, HostGate>::new();
        let a = directory.insert(OwnerIndex::issue(DomainKind::Process).unwrap(), 0).unwrap();
        let b = directory.insert(OwnerIndex::issue(DomainKind::Process).unwrap(), 0).unwrap();
        for (ledger, payload) in directory.entries_mut()
        {
            *payload = ledger.owner().incarnation();
            if ledger.owner() == a
            {
                ledger.stop_admission();
            }
        }
        assert_eq!(*directory.get(a.incarnation()).unwrap().value(), a.incarnation());
        assert!(!directory.get(a.incarnation()).unwrap().ledger().accepting());
        assert_eq!(*directory.get(b.incarnation()).unwrap().value(), b.incarnation());
        assert!(directory.get(b.incarnation()).unwrap().ledger().accepting());
        assert_eq!(directory.iter().map(DomainEntry::owner).collect::<Vec<_>>(), [a, b]);
    }
    #[test]
    fn scoped_reuse_rotation_and_retirement()
    {
        let owner = DomainId::issue(DomainKind::Process).unwrap();
        let mut pool = DomainSlots::new(owner,
                                        KindCode(*b"CTRL"),
                                        CapacityPolicy::PerDomainQuota { slots: 2 }).unwrap();
        pool.grow_to(2).unwrap();
        let old = pool.allocate(owner).unwrap();
        let released = pool.invalidate(old).unwrap();
        pool.complete_release(released).unwrap();
        let next = pool.allocate(owner).unwrap();
        assert_ne!(old.slot(), next.slot());
        let released = pool.invalidate(next).unwrap();
        pool.complete_release(released).unwrap();
        let reused = pool.allocate(owner).unwrap();
        assert_eq!(old.slot(), reused.slot());
        assert!(!pool.contains(old));
        let released = pool.invalidate(reused).unwrap();
        pool.complete_release(released).unwrap();
        pool.generations[0] = u64::MAX;
        pool.cursor = 0;
        let last = pool.allocate(owner).unwrap();
        let released = pool.invalidate(last).unwrap();
        pool.complete_release(released).unwrap();
        assert_eq!(pool.retired(), 1);
        assert!(pool.allocate(owner).unwrap().slot() != 0);
        assert!(matches!(pool.allocate(owner),
                         Err(PoolError::Exhausted { used: 1,
                                                    retired: 1,
                                                    limit: 2,
                                                    .. })));
        let restarted = DomainId::issue(DomainKind::Process).unwrap();
        let mut fresh = DomainSlots::new(restarted,
                                         KindCode(*b"CTRL"),
                                         CapacityPolicy::PerDomainQuota { slots: 2 }).unwrap();
        fresh.grow_to(2).unwrap();
        assert!(!fresh.contains(old));
        assert_eq!(fresh.retired(), 0);
        assert!(matches!(fresh.allocate(owner), Err(PoolError::ForeignOwner { .. })));
    }
    #[test]
    fn invalidate_first_and_stop_without_allocating()
    {
        let owner = DomainId::issue(DomainKind::Module).unwrap();
        let mut pool = DomainSlots::new(owner,
                                        KindCode(*b"REGI"),
                                        CapacityPolicy::GrowableUntilMemory).unwrap();
        pool.grow_to(65).unwrap();
        let key = pool.allocate(owner).unwrap();
        pool.stop_admission();
        let ticket = pool.invalidate_next().unwrap();
        assert_eq!(ticket.key(), key);
        assert!(!pool.contains(key));
        assert!(pool.invalidate_next().is_none());
        assert!(matches!(pool.allocate(owner), Err(PoolError::Stopping { .. })));
        assert!(matches!(pool.invalidate(key), Err(PoolError::Stale { .. })));
    }
    #[test]
    fn index_is_unique_and_growth_is_not_a_quota_failure()
    {
        let mut index = OwnerIndex::issue(DomainKind::Process).unwrap();
        let owner = index.owner();
        let kind = KindCode(*b"WIND");
        index.add_pool(kind, CapacityPolicy::GrowableUntilMemory).unwrap();
        assert!(matches!(index.allocate(kind),
                         Err(PoolError::GrowthRequired { storage_slots: 0,
                                                         policy_limit: None,
                                                         .. })));
        index.grow(kind, 1).unwrap();
        let old = index.allocate(kind).unwrap();
        let released = index.invalidate(old).unwrap();
        index.complete_release(released).unwrap();
        assert!(matches!(index.add_pool(kind, CapacityPolicy::GrowableUntilMemory),
                         Err(PoolError::DuplicateKind { .. })));
        let new = index.allocate(kind).unwrap();
        assert_ne!(new, old);
        assert!(!index.contains(old));
        index.stop_admission();
        let released = index.invalidate_next().unwrap();
        assert_eq!(released.key(), new);
        index.complete_release(released).unwrap();
        assert_eq!(index.live(), 0);
        assert!(matches!(index.add_pool(KindCode(*b"MENU"), CapacityPolicy::GrowableUntilMemory),
                         Err(PoolError::Stopping { .. })));
        let fresh = OwnerIndex::issue(DomainKind::Process).unwrap();
        assert_ne!(owner, fresh.owner());
    }
    #[test]
    fn bitmap_boundaries_growth_and_failed_growth_preserve_history()
    {
        let mut index = OwnerIndex::issue(DomainKind::Process).unwrap();
        let kind = KindCode(*b"TEST");
        index.add_pool(kind, CapacityPolicy::PerDomainQuota { slots: 130 }).unwrap();
        index.grow(kind, 65).unwrap();
        let old: Vec<_> = (0..65).map(|_| index.allocate(kind).unwrap()).collect();
        assert!(matches!(index.allocate(kind),
                         Err(PoolError::GrowthRequired { storage_slots: 65,
                                                         policy_limit: Some(130),
                                                         .. })));
        let error = index.grow(kind, 131).unwrap_err();
        assert!(matches!(error,
                         PoolError::GrowthLimit { requested_slots: 131,
                                                  limit: 130,
                                                  .. }));
        assert!(alloc::format!("{error}").contains("131 requested slots exceed the 130 slot \
                                                    policy limit"));
        assert!(old.iter().all(|&key| index.contains(key)));
        index.grow(kind, 130).unwrap();
        let extra: Vec<_> = (0..65).map(|_| index.allocate(kind).unwrap()).collect();
        assert_eq!(extra[0].slot(), 65);
        assert_eq!(extra[64].slot(), 129);
        assert!(matches!(index.allocate(kind),
                         Err(PoolError::Exhausted { used: 130,
                                                    limit: 130,
                                                    .. })));
        for &key in &old
        {
            let released = index.invalidate(key).unwrap();
            index.complete_release(released).unwrap();
        }
        for expected in old
        {
            let key = index.allocate(kind).unwrap();
            assert_eq!(key.slot(), expected.slot());
            assert_ne!(key, expected);
            assert!(!index.contains(expected));
        }
        index.stop_admission();
        let mut count = 0;
        while let Some(released) = index.invalidate_next()
        {
            index.complete_release(released).unwrap();
            count += 1;
        }
        assert_eq!(count, 130);
        assert_eq!(index.live(), 0);
        assert!(index.invalidate_next().is_none());
    }
    #[test]
    fn system_index_is_never_recreated_after_drop()
    {
        let index = OwnerIndex::issue(DomainKind::System).unwrap();
        assert_eq!(index.owner(), DomainId::SYSTEM);
        drop(index);
        assert!(matches!(OwnerIndex::issue(DomainKind::System),
                         Err(IndexError::Pool(PoolError::SystemIndexAlreadyIssued))));
    }
    #[test]
    fn incarnation_never_wraps_and_hard_bounds_require_reason()
    {
        let counter = AtomicU64::new(u64::MAX);
        assert_eq!(issue(&counter).unwrap().get(), u64::MAX);
        assert_eq!(issue(&counter), Err(IncarnationsExhausted));
        assert!(matches!(DomainSlots::new(DomainId::SYSTEM,
                                          KindCode(*b"TEST"),
                                          CapacityPolicy::HardBound { slots:  1,
                                                                      reason: "" }),
                         Err(PoolError::InvalidBoundReason)));
    }
}

#[cfg(test)]
mod pool_snapshot_tests
{
    use alloc::string::ToString;

    use super::*;
    #[test]
    fn policy_and_generation_headroom_are_authoritative_without_read_mutation()
    {
        for policy in [CapacityPolicy::GrowableUntilMemory,
                       CapacityPolicy::PerDomainQuota { slots: 1 },
                       CapacityPolicy::HardBound { slots:  1,
                                                   reason: "one hardware channel" }]
        {
            let mut ledger = OwnerIndex::issue(DomainKind::Process).unwrap();
            let kind = KindCode(*b"SNAP");
            ledger.add_pool(kind, policy).unwrap();
            ledger.grow(kind, 1).unwrap();
            let epoch = ledger.summary().revision;
            let empty = ledger.pool_at(epoch, 0).unwrap();
            assert_eq!(empty.policy, policy);
            assert_eq!(empty.generation_headroom, u64::MAX);
            assert_eq!(ledger.summary().revision, epoch);
            assert!(matches!(ledger.pool_at(epoch, 1),
                             Err(LedgerReadError::PoolNotFound { index: 1,
                                                                 .. })));
            // Set up the last issuable generation; allocation/invalidation still
            // update the same high-water mechanism used by ordinary resources.
            ledger.pool_mut(kind).unwrap().generations[0] = u64::MAX;
            let key = ledger.allocate(kind).unwrap();
            assert!(matches!(ledger.pool_at(epoch, 0), Err(LedgerReadError::Stale { .. })));
            let full = ledger.pool_at(ledger.summary().revision, 0).unwrap();
            assert_eq!((full.live, full.births, full.generation_headroom), (1, 1, 0));
            let ticket = ledger.invalidate(key).unwrap();
            let pending = ledger.pool_at(ledger.summary().revision, 0).unwrap();
            assert_eq!((pending.live, pending.retired, pending.pending, pending.deaths),
                       (0, 1, 1, 1));
            ledger.complete_release(ticket).unwrap();
            let complete = ledger.pool_at(ledger.summary().revision, 0).unwrap();
            assert_eq!((complete.pending, complete.generation_headroom), (0, 0));
            ledger.revision = u64::MAX;
            let error = ledger.pool_at(u64::MAX, 0).unwrap_err();
            assert!(matches!(error, LedgerReadError::RevisionExhausted { .. }));
            assert!(error.to_string().contains("restart this domain"));
        }
    }
}

#[cfg(test)]
mod completion_tests
{
    use alloc::{string::ToString, sync::Arc};
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::{registry::{CleanupFailure, Registry, Release, Source, Specification},
                shared::{InvocationGate, OwnerRef},
                store::Store,
                walk};
    struct Gate;
    impl InvocationGate for Gate
    {
        fn run<R>(operation: impl FnOnce() -> R) -> R
        {
            operation()
        }
    }
    struct Cleanup;
    impl Release<()> for Cleanup
    {
        fn release(&self, _: &mut (), ticket: ReleasedResource) -> Result<(), CompletionError>
        {
            Err(CompletionError::registration_mismatch(ticket))
        }
    }
    struct Payload(Arc<AtomicUsize>);
    impl Drop for Payload
    {
        fn drop(&mut self)
        {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    #[test]
    fn refused_completion_retains_unique_ticket_and_pin_then_retries_only_acknowledgement()
    {
        let registrar =
            OwnerRef::<Gate>::try_new(OwnerIndex::issue(DomainKind::Module).unwrap()).unwrap();
        let owner =
            OwnerRef::<Gate>::try_new(OwnerIndex::issue(DomainKind::Process).unwrap()).unwrap();
        let kind = KindCode(*b"FAIL");
        let mut registry = Registry::<(), Gate>::new();
        let lease = registry.register(&registrar,
                              Specification { kind,
                                              label: "Fault fixture",
                                              provider: "Fixture provider",
                                              capacity: CapacityPolicy::PerDomainQuota { slots: 1 },
                                              source: Source::Resident },
                              Cleanup)
                    .unwrap();
        let registration = lease.id();
        let mut store = Store::registered(lease);
        let drops = Arc::new(AtomicUsize::new(0));
        let id = store.insert(&owner, Payload(drops.clone())).unwrap();
        let ticket = owner.invalidate_next().unwrap();
        let key = ticket.key();
        let removed = store.release(ticket).unwrap();
        // Trusted metadata fault injection: the payload still owns the real,
        // unique ticket. No duplicate capability or synthetic error is made.
        owner.access(|index| index.pool_mut(kind).unwrap().registration = Some(registration + 1));
        drop(removed);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        let state = owner.summary();
        assert_eq!((state.live, state.pending, state.completion_faults), (0, 1, 1));
        let fault = owner.completion_fault_at(state.revision, 0).unwrap().unwrap();
        assert_eq!(fault.key, key);
        assert_eq!(fault.cause, crate::completion::Cause::Refused(PoolError::Stale { key }));
        assert!(fault.to_string().contains("ticket and provider pin remain retained for retry"));
        assert_eq!(owner.pool_at(state.revision, 0).unwrap().completion_faults, 1);
        assert!(matches!(registry.unregister(&registrar, kind),
                         Err(crate::registry::Error::Busy { remaining: 1,
                                                            .. })));
        assert!(matches!(owner.retire(),
                         Err(DirectoryError::CompletionFaults { faults: 1,
                                                                .. })));
        // Restore the trusted fault. Retry must never run the destructor again.
        owner.access(|index| index.pool_mut(kind).unwrap().registration = Some(registration));
        let report =
            walk::retry(&mut (),
                        &owner,
                        |_, _| -> Result<crate::registry::Invocation<(), Gate>, ()> {
                            panic!("destroyed payload must not be dispatched again")
                        },
                        |_, error| panic!("restored completion must succeed: {error:?}")).unwrap();
        assert_eq!(report.released, 1);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!((owner.summary().pending, owner.summary().completion_faults), (0, 0));
        assert!(matches!(owner.completion_fault_at(state.revision, 0),
                         Err(LedgerReadError::Stale { .. })));
        assert!(store.get(id).is_none());
        drop(store);
        drop(registry.unregister(&registrar, kind).unwrap());
        owner.retire().unwrap();
    }
    #[test]
    fn projected_completion_retry_releases_the_same_treaty_slot_only_after_native_ack()
    {
        let registrar =
            OwnerRef::<Gate>::try_new(OwnerIndex::issue(DomainKind::Module).unwrap()).unwrap();
        let owner =
            OwnerRef::<Gate>::try_new(OwnerIndex::issue(DomainKind::Process).unwrap()).unwrap();
        let kind = KindCode(*b"FAIL");
        let mut registry = Registry::<(), Gate>::new();
        let lease = registry.register(&registrar,
                              Specification { kind,
                                              label: "Projected fault",
                                              provider: "Projection fixture",
                                              capacity: CapacityPolicy::PerDomainQuota { slots: 1 },
                                              source: Source::Resident },
                              Cleanup)
                    .unwrap();
        let registration = lease.id();
        let storage =
            registry.register(&registrar,
                              Specification { kind:     KindCode(*b"FAKB"),
                                              label:    "Projection backing",
                                              provider: "Projection fixture",
                                              capacity: CapacityPolicy::PerDomainQuota { slots: 1 },
                                              source:   Source::Resident },
                              Cleanup)
                    .unwrap();
        let mut store = crate::projected::Store::new(crate::Kind::Files, lease, storage).unwrap();
        let drops = Arc::new(AtomicUsize::new(0));
        let old = store.insert(&owner, Payload(drops.clone())).unwrap();
        let ticket = owner.invalidate_next().unwrap();
        assert_eq!(ticket.key().kind(), kind);
        let removed = store.release(ticket).unwrap();
        owner.access(|index| index.pool_mut(kind).unwrap().registration = Some(registration + 1));
        drop(removed);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!((owner.summary().pending, owner.summary().completion_faults), (1, 1));
        assert!(store.get(old).is_none());
        assert_eq!(registry.by_id(registry.snapshot().unwrap().0, registration).unwrap().uses, 1);
        let refused = store.insert(&owner, Payload(drops.clone())).unwrap_err();
        assert!(matches!(refused,
                         crate::projected::Error::Pool(PoolError::Exhausted { pending: 1,
                                                                              .. })));
        assert!(refused.to_string().contains("pending cleanup"));
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        owner.access(|index| index.pool_mut(kind).unwrap().registration = Some(registration));
        let retried = walk::retry(&mut (),
                                  &owner,
                                  |_, _| -> Result<crate::registry::Invocation<(), Gate>, ()> {
                                      panic!("destroyed projection must not run again")
                                  },
                                  |_, error| {
                                      panic!("restored projection acknowledgement refused: \
                                              {error:?}")
                                  }).unwrap();
        assert_eq!(retried.released, 1);
        assert_eq!((owner.summary().pending, owner.summary().completion_faults), (0, 0));
        assert_eq!(registry.by_id(registry.snapshot().unwrap().0, registration).unwrap().uses, 0);
        let fresh = store.insert(&owner, Payload(drops.clone())).unwrap();
        assert_ne!(fresh, old);
        assert_eq!(fresh & 0xffff,
                   old & 0xffff,
                   "retry leaked the retained treaty slot instead of releasing it");
        assert!(store.get(old).is_none());
        assert!(store.get(fresh).is_some());
        drop(store.remove(fresh));
        drop(store);
        assert_eq!(drops.load(Ordering::Relaxed), 3);
        assert_eq!((owner.summary().live,
                    owner.summary().pending,
                    owner.summary().completion_faults),
                   (0, 0, 0));
    }

    #[test]
    fn backing_destroyed_before_dispatcher_returns_ticket_never_acknowledges_early()
    {
        let owner =
            OwnerRef::<Gate>::try_new(OwnerIndex::issue(DomainKind::Process).unwrap()).unwrap();
        let kind = KindCode(*b"WAIT");
        owner.add_pool(kind, CapacityPolicy::PerDomainQuota { slots: 1 }).unwrap();
        owner.grow(kind, 1).unwrap();
        let key = owner.allocate(kind).unwrap();
        let mut completion =
            crate::completion::Reserve::try_new().unwrap().live(owner.clone(), key, None);
        let ticket = owner.invalidate(key).unwrap();
        completion.run(|| {});
        assert_eq!((owner.summary().pending, owner.summary().completion_faults), (1, 1));
        assert_eq!(owner.completion_fault_at(owner.summary().revision, 0).unwrap().unwrap().cause,
                   crate::completion::Cause::AwaitingTicket);
        owner.quarantine(ticket).unwrap();
        let report = walk::retry(&mut (),
                                 &owner,
                                 |_, _| -> Result<crate::registry::Invocation<(), Gate>, ()> {
                                     panic!("destroyed backing has no remaining callback")
                                 },
                                 |_, error| {
                                     panic!("waiting for a ticket is successful deferral: \
                                             {error:?}")
                                 }).unwrap();
        assert_eq!((report.released, report.deferred), (1, 1));
        assert_eq!((owner.summary().pending, owner.summary().completion_faults), (0, 0));
        assert_eq!(owner.allocate(kind).unwrap().slot(), key.slot());
        assert!(!owner.contains(key));
    }
}
