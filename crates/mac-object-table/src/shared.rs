//! Shared owner authority. The selected architecture gate encloses every lock
//! operation; callers never receive a mutable index or an escaping lock guard.
use alloc::sync::Arc;
use core::{marker::PhantomData,
           sync::atomic::{AtomicBool, Ordering}};

use spinning_top::Spinlock;

use crate::domain::{CapacityPolicy, CompletionError, DirectoryError, DomainId, DomainKind,
                    IndexError, KindCode, LedgerReadError, LedgerSummary, OwnerIndex, PoolError,
                    PoolSummary, QuarantineRetry, ReleasedResource, ResourceKey, UnpublishedIndex};

/// The resident adapter must mask local interrupts/preemption before calling
/// the closure, and restore the previous state afterward, including nesting.
/// The metadata closure is synchronous and never invokes module/app callbacks.
/// Host implementations have no interrupts; concurrency still uses the lock.
/// There is deliberately no default/no-op architecture selection.
pub trait InvocationGate
{
    fn run<R>(operation: impl FnOnce() -> R) -> R;

    /// Attribute backing for shared resident indexes and System-owned objects
    /// to the platform's ordinary System allocation authority. This changes
    /// only new allocations in this bounded operation; existing backing keeps
    /// its original refund destination. It neither authenticates an owner nor
    /// opens a ledger lock. Platforms without domain allocation attribution
    /// use the neutral default.
    fn with_system_allocation<R>(operation: impl FnOnce() -> R) -> R
    {
        operation()
    }

    /// Attribute only the canonical owner/account authority control blocks to
    /// System foundation. These establish authority and cannot carry receipts
    /// from the account they create. Per-domain Claims, ledger arrays, slots
    /// and payloads are ordinary owner allocations. As above, the neutral
    /// default supports platforms without domain allocation attribution.
    fn with_foundation_allocation<R>(operation: impl FnOnce() -> R) -> R
    {
        operation()
    }

    /// A stopped ledger has actually completed cleanup. Called after payload
    /// destruction and metadata unlock; the resident adapter may post a cheap,
    /// coalesced retirement wake. This must not allocate or run cleanup itself.
    fn released(_owner: DomainId)
    {
    }

    /// A published operation's unique owner has closed its admission. Called
    /// after metadata unlock; post only a coalesced notification, never cleanup.
    fn scope_closed(_owner: DomainId)
    {
    }
}

mod sealed
{
    pub trait Ledger
    {
    }
    impl Ledger for super::OwnerIndex
    {
    }
    impl<G: super::InvocationGate> Ledger for super::OwnerRef<G>
    {
    }
}

/// Read access to an issuer-owned ledger. Sealing prevents a caller from
/// fabricating a validator that accepts keys belonging to a different subject.
/// ```compile_fail
/// use mac_object_table::{domain::{DomainId, ResourceKey}, shared::OwnerLedger};
/// struct Forged;
/// impl OwnerLedger for Forged {
///     fn owner(&self) -> DomainId { DomainId::SYSTEM }
///     fn contains(&self, _: ResourceKey) -> bool { true }
///     fn accepting(&self) -> bool { true }
/// }
/// ```
pub trait OwnerLedger: sealed::Ledger
{
    fn owner(&self) -> DomainId;
    fn contains(&self, key: ResourceKey) -> bool;
    fn accepting(&self) -> bool;
}
impl OwnerLedger for OwnerIndex
{
    fn owner(&self) -> DomainId
    {
        OwnerIndex::owner(self)
    }

    fn contains(&self, key: ResourceKey) -> bool
    {
        OwnerIndex::contains(self, key)
    }

    fn accepting(&self) -> bool
    {
        OwnerIndex::accepting(self)
    }
}
impl<G: InvocationGate> OwnerLedger for OwnerRef<G>
{
    fn owner(&self) -> DomainId
    {
        OwnerRef::owner(self)
    }

    fn contains(&self, key: ResourceKey) -> bool
    {
        OwnerRef::contains(self, key)
    }

    fn accepting(&self) -> bool
    {
        OwnerRef::accepting(self)
    }
}

struct State<G: InvocationGate>
{
    owner:     DomainId,
    admission: Arc<AtomicBool>,
    index:     Spinlock<OwnerIndex>,
    gate:      PhantomData<fn() -> G>
}

/// Clones refer to the same authoritative ledger, never a new identity issuer.
/// Keeping this reference does not keep a stopped domain open for admission.
pub struct OwnerRef<G: InvocationGate>
{
    state: Arc<State<G>>
}
impl<G: InvocationGate> Clone for OwnerRef<G>
{
    fn clone(&self) -> Self
    {
        Self { state: self.state.clone() }
    }
}
impl<G: InvocationGate> OwnerRef<G>
{
    /// Allocate the complete shared root before publishing its namespace. A
    /// backing refusal leaves the unique System issuer available for retry.
    pub fn issue(kind: DomainKind) -> Result<Self, IndexError>
    {
        // Preparation allocates only the admission control block; conversion
        // below separately scopes its canonical shared root allocation.
        G::with_foundation_allocation(|| UnpublishedIndex::prepare(kind))?.into_shared()
    }

    /// Share an already issued index. Issuance remains irreversible even when
    /// this separate allocation fails; use `issue` to create a new shared root.
    pub fn try_new(index: OwnerIndex) -> Result<Self, DirectoryError>
    {
        let owner = index.owner();
        let admission = index.admission();
        let state = G::with_foundation_allocation(|| {
                        Arc::try_new(State { owner,
                                             admission,
                                             index: Spinlock::new(index),
                                             gate: PhantomData })
                    }).map_err(|_| DirectoryError::Storage { owner })?;
        Ok(Self { state })
    }

    pub fn owner(&self) -> DomainId
    {
        self.state.owner
    }

    pub(crate) fn access<R>(&self, operation: impl FnOnce(&mut OwnerIndex) -> R) -> R
    {
        G::run(|| operation(&mut self.state.index.lock()))
    }

    pub fn add_pool(&self, kind: KindCode, policy: CapacityPolicy) -> Result<(), PoolError>
    {
        self.access(|index| index.add_pool(kind, policy))
    }

    pub fn grow(&self, kind: KindCode, slots: usize) -> Result<(), PoolError>
    {
        self.access(|index| index.grow(kind, slots))
    }

    pub fn allocate(&self, kind: KindCode) -> Result<ResourceKey, PoolError>
    {
        self.access(|index| index.allocate(kind))
    }

    pub fn contains(&self, key: ResourceKey) -> bool
    {
        self.access(|index| index.contains(key))
    }

    pub fn pending(&self, key: ResourceKey) -> bool
    {
        self.access(|index| index.pending(key))
    }

    pub fn parent(&self, key: ResourceKey) -> Option<ResourceKey>
    {
        self.access(|index| index.parent(key))
    }

    pub fn invalidate(&self, key: ResourceKey) -> Result<ReleasedResource, PoolError>
    {
        self.access(|index| index.invalidate(key))
    }

    pub fn complete_release(&self, released: ReleasedResource) -> Result<(), CompletionError>
    {
        let stopped = self.access(|index| {
                              index.complete_release(released)?;
                              Ok(!index.accepting())
                          })?;
        if stopped
        {
            G::released(self.owner());
        }
        Ok(())
    }

    pub(crate) fn complete_value_release(&self, released: ReleasedResource)
                                         -> Result<bool, CompletionError>
    {
        self.access(|index| {
                index.complete_value_release(released)?;
                Ok(!index.accepting())
            })
    }

    pub(crate) fn notify_retirement(&self)
    {
        if !self.accepting()
        {
            G::released(self.owner());
        }
    }

    pub fn quarantine(&self, released: ReleasedResource) -> Result<(), CompletionError>
    {
        self.access(|index| index.quarantine(released))
    }

    pub fn resume_quarantined(&self) -> Option<ReleasedResource>
    {
        self.access(OwnerIndex::resume_quarantined)
    }

    pub(crate) fn quarantine_retry(&self) -> QuarantineRetry
    {
        self.access(|index| index.quarantine_retry())
    }

    pub(crate) fn resume_quarantined_in(&self, cursor: &mut QuarantineRetry)
                                        -> Option<ReleasedResource>
    {
        self.access(|index| index.resume_quarantined_in(cursor))
    }

    pub fn live(&self) -> usize
    {
        self.access(|index| index.live())
    }

    pub fn summary(&self) -> LedgerSummary
    {
        self.access(|index| index.summary())
    }

    /// Atomic root-release barrier. Unknown future kinds count automatically;
    /// a manager excludes only its own root/backing, never a cleanup roll call.
    pub fn has_resources_outside(&self, excluded: &[KindCode]) -> bool
    {
        self.access(|index| {
                index.pools().any(|pool| {
                                 !excluded.contains(&pool.kind())
                                 && (pool.live() != 0 || pool.pending() != 0)
                             })
                || index.summary().completion_faults != 0
            })
    }

    /// The revision check and value copy share the same invocation/metadata
    /// lock. Callers receive neither a mutable pool nor an escaping guard.
    pub fn pool_at(&self, revision: u64, at: usize) -> Result<PoolSummary, LedgerReadError>
    {
        self.access(|index| index.pool_at(revision, at))
    }

    pub fn stop_admission(&self)
    {
        self.access(OwnerIndex::stop_admission);
    }

    pub fn accepting(&self) -> bool
    {
        self.state.admission.load(Ordering::Acquire)
    }

    /// Release empty stopped-owner ledger storage before an account's death
    /// barrier. Destruction/refunds occur after the invocation lock is released.
    /// Retrying after a successful drain is harmless and never reopens admission.
    pub fn drain_stopped(&self) -> Result<(), DirectoryError>
    {
        let pools = self.access(OwnerIndex::drain_stopped)?;
        drop(pools);
        Ok(())
    }

    pub fn invalidate_next(&self) -> Option<ReleasedResource>
    {
        self.access(OwnerIndex::invalidate_next)
    }

    pub(crate) fn retain_completion(&self, record: alloc::boxed::Box<crate::completion::Record>)
    {
        self.access(|index| index.retain_completion(record));
    }

    pub(crate) fn retry_completion(&self) -> crate::completion::Retry
    {
        self.access(|index| index.retry_completion())
    }

    pub(crate) fn route_completion(&self, ticket: ReleasedResource) -> crate::completion::Routed
    {
        self.access(|index| index.route_completion(ticket))
    }

    pub fn completion_fault_at(&self, revision: u64, at: usize)
                               -> Result<Option<crate::completion::Fault>, LedgerReadError>
    {
        self.access(|index| index.completion_fault_at(revision, at))
    }

    pub(crate) fn retire(&self) -> Result<(), DirectoryError>
    {
        let pools = self.access(OwnerIndex::retire)?;
        drop(pools);
        Ok(())
    }
}
