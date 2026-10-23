//! Process-owned control zones and the host allocator-forwarding contract.
//! Neither a definition nor a module supplies an app pointer to the host.
use core::alloc::Layout;

use crate::{DomainId, ResourceKey,
            policy::{BudgetExceeded, ConfiguredRange, ModelError}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZoneBinding
{
    instance: ResourceKey,
    zone:     ResourceKey
}
impl ZoneBinding
{
    pub fn new(instance: ResourceKey, zone: ResourceKey) -> Result<Self, ModelError>
    {
        if instance.owner() != zone.owner()
        {
            return Err(ModelError::WrongDomain);
        }
        if instance == zone
        {
            return Err(ModelError::InvalidBinding);
        }
        Ok(Self { instance,
                  zone })
    }

    pub const fn owner(self) -> DomainId
    {
        self.instance.owner()
    }

    pub const fn instance(self) -> ResourceKey
    {
        self.instance
    }

    pub const fn zone(self) -> ResourceKey
    {
        self.zone
    }
}
pub struct ControlZone
{
    binding: ZoneBinding,
    limit:   ConfiguredRange
}
impl ControlZone
{
    pub const fn new(binding: ZoneBinding, limit: ConfiguredRange) -> Self
    {
        Self { binding,
               limit }
    }

    pub const fn binding(&self) -> ZoneBinding
    {
        self.binding
    }

    pub const fn limit(&self) -> ConfiguredRange
    {
        self.limit
    }
}
/// A service call records this context on behalf of the caller. It is not a
/// caller-controlled authority; the resident gate validates keys every time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocationContext
{
    System,
    Kernel(DomainId),
    Application(DomainId),
    Control(ZoneBinding)
}
/// Compile-time host implementation. `ForwardingLease::allocate` returns an
/// owned, releasing lease;
/// separately linked modules reach it through an audited ABI adapter, never
/// through a Rust vtable. There is no process-global current-zone variable.
///
/// The raw reserve/release pair belongs to the host allocator implementation,
/// not module callers. Reserve must charge before publishing storage and undo
/// the charge on failure; release returns storage and its account charge.
/// Consumers use the concrete lease, which owns the release call. There is no
/// implementation-selected lease type whose destructor a caller must trust.
/// No global allocator is installed by this crate.
pub trait ForwardingAllocator
{
    type Storage;
    fn reserve(&self, context: AllocationContext, layout: Layout)
               -> Result<Self::Storage, BudgetExceeded>;
    fn release(&self, context: AllocationContext, layout: Layout, storage: Self::Storage);
}

/// Owned forwarding allocation: moving it transfers the sole release
/// obligation, and ordinary scope exit (including unwinding) releases once.
/// It borrows the allocator, so the allocator cannot die ahead of its lease.
///
/// Rust permits `mem::forget`; the live zone ledger must also retain allocation
/// ownership and reap forgotten leases when disposing the instance. Drop is
/// eager cleanup, not the domain's only reclamation mechanism.
///
/// A lease cannot outlive its allocator:
/// ```compile_fail
/// use mac_kernel_model::zone::{AllocationContext, ForwardingAllocator, ForwardingLease};
/// fn escape<A: ForwardingAllocator>(allocator: &A) -> ForwardingLease<'static, A> {
///     ForwardingLease::allocate(allocator, AllocationContext::System,
///                               core::alloc::Layout::new::<u64>()).unwrap()
/// }
/// ```
#[must_use = "dropping the lease releases its storage and account charge"]
pub struct ForwardingLease<'a, A: ForwardingAllocator + ?Sized>
{
    allocator: &'a A,
    context:   AllocationContext,
    layout:    Layout,
    storage:   Option<A::Storage>
}
impl<'a, A: ForwardingAllocator + ?Sized> ForwardingLease<'a, A>
{
    pub fn allocate(allocator: &'a A, context: AllocationContext, layout: Layout)
                    -> Result<Self, BudgetExceeded>
    {
        let storage = allocator.reserve(context, layout)?;
        Ok(Self { allocator,
                  context,
                  layout,
                  storage: Some(storage) })
    }

    pub const fn context(&self) -> AllocationContext
    {
        self.context
    }

    pub const fn layout(&self) -> Layout
    {
        self.layout
    }
}
impl<A: ForwardingAllocator + ?Sized> Drop for ForwardingLease<'_, A>
{
    fn drop(&mut self)
    {
        if let Some(storage) = self.storage.take()
        {
            self.allocator.release(self.context, self.layout, storage);
        }
    }
}
