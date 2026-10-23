//! Lifetime-domain identity, the resident process owner directory, and ledger
//! metadata. Other managers migrate under memory/app/resource spec §13.
//! Native/legacy wire projections remain the responsibility of the facility.
use alloc::vec::Vec;
use core::{fmt,
           num::NonZeroU64,
           sync::atomic::{AtomicBool, AtomicU64, Ordering}};

use crate::shared::{InvocationGate, OwnerRef};

static SYSTEM_INDEX_ISSUED: AtomicBool = AtomicBool::new(false);
static NEXT_INCARNATION: AtomicU64 = AtomicU64::new(1);
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
        f.write_str("boot-wide 64-bit domain identity exhausted; an incarnation cannot be reused")
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
    owner:      DomainId,
    kind:       KindCode,
    slot:       usize,
    generation: NonZeroU64
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
    InvalidBoundReason,
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
impl fmt::Display for PoolError
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
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
                                   storage_slots,
                                   policy_limit } =>
            {
                write!(f,
                       "'{kind}' metadata growth required for {owner}: {used} live, {retired} \
                        retired, {storage_slots} prepared slots, policy limit {policy_limit:?}")
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
                              limit } =>
            {
                write!(f,
                       "'{kind}' handle pool exhausted in {owner}: {used} live, {retired} \
                        retired, {limit} slot limit")
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
    owner:         DomainId,
    kind:          KindCode,
    policy:        CapacityPolicy,
    occupied:      Vec<u64>,
    retired:       Vec<u64>,
    generations:   Vec<u64>,
    cursor:        usize,
    live:          usize,
    retired_count: usize,
    births:        u64,
    deaths:        u64,
    accepting:     bool,
    drain_word:    usize
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
                  generations: Vec::new(),
                  cursor: 0,
                  live: 0,
                  retired_count: 0,
                  births: 0,
                  deaths: 0,
                  accepting: true,
                  drain_word: 0 })
    }

    /// Capacity comes from the caller's reviewed policy; there is no hidden
    /// initial size or growth increment. All fallible work precedes publication.
    pub fn grow_to(&mut self, slots: usize) -> Result<(), PoolError>
    {
        if !self.accepting
        {
            return Err(PoolError::Stopping { owner: self.owner });
        }
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
        self.generations.try_reserve(slots - self.generations.len()).map_err(|_| error)?;
        self.occupied.resize(words, 0);
        self.retired.resize(words, 0);
        self.generations.resize(slots, 0);
        Ok(())
    }

    fn exhausted(&self) -> PoolError
    {
        PoolError::Exhausted { owner:   self.owner,
                               kind:    self.kind,
                               used:    self.live,
                               retired: self.retired_count,
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
        self.check_owner(owner)?;
        if !self.accepting
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
                let free = !(self.occupied[word] | self.retired[word]) & span;
                if free != 0
                {
                    let slot = word * 64 + free.trailing_zeros() as usize;
                    let generation = NonZeroU64::new(self.generations[slot].max(1)).unwrap();
                    self.generations[slot] = generation.get();
                    self.occupied[word] |= 1 << (slot % 64);
                    self.live += 1;
                    self.births = self.births.saturating_add(1);
                    self.drain_word = self.drain_word.min(word);
                    self.cursor = if slot + 1 == len { 0 } else { slot + 1 };
                    return Ok(ResourceKey { owner,
                                            kind: self.kind,
                                            slot,
                                            generation });
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
        self.live -= 1;
        self.deaths = self.deaths.saturating_add(1);
        match self.generations[key.slot].checked_add(1)
        {
            Some(next) => self.generations[key.slot] = next,
            None =>
            {
                self.retired[key.slot / 64] |= mask;
                self.retired_count += 1;
            }
        }
        Ok(ReleasedResource { key })
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
                                generation: NonZeroU64::new(self.generations[slot])? };
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
pub struct ReleasedResource
{
    key: ResourceKey
}
impl ReleasedResource
{
    pub const fn key(&self) -> ResourceKey
    {
        self.key
    }
}
/// Central owner index shape; registering pools/dispatchers and draining queues
/// are the next runtime migration. No manager-specific cleanup list belongs here.
pub struct OwnerIndex
{
    owner:     DomainId,
    pools:     Vec<DomainSlots>,
    accepting: bool,
    revision:  u64
}

/// One locked read of the authoritative ledger; no object-value scan or allocation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LedgerSummary
{
    pub revision:  u64,
    pub kinds:     usize,
    pub live:      usize,
    pub retired:   usize,
    pub slots:     usize,
    pub births:    u64,
    pub deaths:    u64,
    pub accepting: bool
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
    }
}
impl fmt::Display for DirectoryError
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Storage { owner } => write!(f, "owner-directory allocation refused for {owner}"),
            Self::NotFound { incarnation } =>
            {
                write!(f, "lifetime domain incarnation {incarnation} not found")
            }
            Self::LiveResources { owner,
                                  live } =>
            {
                write!(f, "cannot retire {owner}: {live} live ledger resources remain")
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
        let ledger = OwnerRef::new(ledger)?;
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
    Pool(PoolError)
}
impl fmt::Display for IndexError
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Identity(e) => e.fmt(f),
            Self::Pool(e) => e.fmt(f)
        }
    }
}
impl OwnerIndex
{
    pub fn summary(&self) -> LedgerSummary
    {
        let mut result = LedgerSummary { revision: self.revision,
                                         kinds: self.pools.len(),
                                         accepting: self.accepting,
                                         ..Default::default() };
        for pool in &self.pools
        {
            result.live += pool.live();
            result.retired += pool.retired();
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
        if kind == DomainKind::System
           && SYSTEM_INDEX_ISSUED.compare_exchange(false,
                                                   true,
                                                   Ordering::Relaxed,
                                                   Ordering::Relaxed)
                                 .is_err()
        {
            return Err(IndexError::Pool(PoolError::SystemIndexAlreadyIssued));
        }
        let owner = DomainId::issue(kind).map_err(IndexError::Identity)?;
        Ok(Self { owner,
                  pools: Vec::new(),
                  accepting: true,
                  revision: 1 })
    }

    pub const fn owner(&self) -> DomainId
    {
        self.owner
    }

    pub const fn accepting(&self) -> bool
    {
        self.accepting
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
        if !self.accepting
        {
            return Err(PoolError::Stopping { owner: self.owner });
        }
        if self.pools.iter().any(|p| p.kind == kind)
        {
            return Err(PoolError::DuplicateKind { owner: self.owner,
                                                  kind });
        }
        let pool = DomainSlots::new(self.owner, kind, policy)?;
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

    pub fn grow(&mut self, kind: KindCode, slots: usize) -> Result<(), PoolError>
    {
        if !self.accepting
        {
            return Err(PoolError::Stopping { owner: self.owner });
        }
        let owner = self.owner;
        self.pool_mut(kind)
            .ok_or(PoolError::UnregisteredKind { owner,
                                                 kind })?
            .grow_to(slots)?;
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    pub fn allocate(&mut self, kind: KindCode) -> Result<ResourceKey, PoolError>
    {
        if !self.accepting
        {
            return Err(PoolError::Stopping { owner: self.owner });
        }
        let owner = self.owner;
        let key = self.pool_mut(kind)
                      .ok_or(PoolError::UnregisteredKind { owner,
                                                           kind })?
                      .allocate(owner)?;
        self.revision = self.revision.saturating_add(1);
        Ok(key)
    }

    pub fn contains(&self, key: ResourceKey) -> bool
    {
        self.pools.iter().find(|p| p.kind == key.kind()).is_some_and(|p| p.contains(key))
    }

    pub fn invalidate(&mut self, key: ResourceKey) -> Result<ReleasedResource, PoolError>
    {
        let released =
            self.pool_mut(key.kind()).ok_or(PoolError::Stale { key })?.invalidate(key)?;
        self.revision = self.revision.saturating_add(1);
        Ok(released)
    }

    pub fn stop_admission(&mut self)
    {
        if self.accepting
        {
            self.revision = self.revision.saturating_add(1);
        }
        self.accepting = false;
        for pool in &mut self.pools
        {
            pool.stop_admission();
        }
    }

    pub(crate) fn retire(&mut self) -> Result<(), DirectoryError>
    {
        let live = self.live();
        if live != 0
        {
            return Err(DirectoryError::LiveResources { owner: self.owner,
                                                       live });
        }
        // Check and close under the caller's single metadata lock. A manager's
        // surviving reference is only a closed identity stub, not a pin on all
        // of the dead domain's bitmap/generation storage. Incarnation uniqueness
        // makes discarding this history safe; this index can never reopen.
        self.stop_admission();
        self.pools = Vec::new();
        Ok(())
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
        index.invalidate(key).unwrap();
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
            OwnerIndex { owner:     DomainId { kind: DomainKind::Process,
                                               incarnation },
                         pools:     Vec::new(),
                         accepting: true,
                         revision:  1 }
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
    fn scoped_reuse_rotation_and_retirement()
    {
        let owner = DomainId::issue(DomainKind::Process).unwrap();
        let mut pool = DomainSlots::new(owner,
                                        KindCode(*b"CTRL"),
                                        CapacityPolicy::PerDomainQuota { slots: 2 }).unwrap();
        pool.grow_to(2).unwrap();
        let old = pool.allocate(owner).unwrap();
        pool.invalidate(old).unwrap();
        let next = pool.allocate(owner).unwrap();
        assert_ne!(old.slot(), next.slot());
        pool.invalidate(next).unwrap();
        let reused = pool.allocate(owner).unwrap();
        assert_eq!(old.slot(), reused.slot());
        assert!(!pool.contains(old));
        pool.invalidate(reused).unwrap();
        pool.generations[0] = u64::MAX;
        pool.cursor = 0;
        let last = pool.allocate(owner).unwrap();
        pool.invalidate(last).unwrap();
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
        index.invalidate(old).unwrap();
        assert!(matches!(index.add_pool(kind, CapacityPolicy::GrowableUntilMemory),
                         Err(PoolError::DuplicateKind { .. })));
        let new = index.allocate(kind).unwrap();
        assert_ne!(new, old);
        assert!(!index.contains(old));
        index.stop_admission();
        assert_eq!(index.invalidate_next().unwrap().key(), new);
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
            index.invalidate(key).unwrap();
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
        while index.invalidate_next().is_some()
        {
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
