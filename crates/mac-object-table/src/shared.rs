//! Shared owner authority. The selected architecture gate encloses every lock
//! operation; callers never receive a mutable index or an escaping lock guard.
use alloc::sync::Arc;
use core::marker::PhantomData;

use spinning_top::Spinlock;

use crate::domain::{CapacityPolicy, CompletionError, DirectoryError, DomainId, KindCode,
                    LedgerSummary, OwnerIndex, PoolError, ReleasedResource, ResourceKey};

/// The resident adapter must mask local interrupts/preemption before calling
/// the closure, and restore the previous state afterward, including nesting.
/// The metadata closure is synchronous and never invokes module/app callbacks.
/// Host implementations have no interrupts; concurrency still uses the lock.
/// There is deliberately no default/no-op architecture selection.
pub trait InvocationGate
{
    fn run<R>(operation: impl FnOnce() -> R) -> R;
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
    owner: DomainId,
    index: Spinlock<OwnerIndex>,
    gate:  PhantomData<fn() -> G>
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
    pub fn try_new(index: OwnerIndex) -> Result<Self, DirectoryError>
    {
        let owner = index.owner();
        let state = Arc::try_new(State { owner,
                                         index: Spinlock::new(index),
                                         gate: PhantomData }).map_err(|_| {
                                                                 DirectoryError::Storage { owner }
                                                             })?;
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

    pub fn invalidate(&self, key: ResourceKey) -> Result<ReleasedResource, PoolError>
    {
        self.access(|index| index.invalidate(key))
    }

    pub fn complete_release(&self, released: ReleasedResource) -> Result<(), CompletionError>
    {
        self.access(|index| index.complete_release(released))
    }

    pub fn live(&self) -> usize
    {
        self.access(|index| index.live())
    }

    pub fn summary(&self) -> LedgerSummary
    {
        self.access(|index| index.summary())
    }

    pub fn stop_admission(&self)
    {
        self.access(OwnerIndex::stop_admission);
    }

    pub fn accepting(&self) -> bool
    {
        self.access(|index| index.accepting())
    }

    pub fn invalidate_next(&self) -> Option<ReleasedResource>
    {
        self.access(OwnerIndex::invalidate_next)
    }

    pub(crate) fn retire(&self) -> Result<(), DirectoryError>
    {
        self.access(OwnerIndex::retire)
    }
}
