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
/// Compile-time host implementation. It returns an owned, releasing lease;
/// separately linked modules reach it through an audited ABI adapter, never
/// through a Rust vtable. There is no process-global current-zone variable.
///
/// Allocation must charge the supplied context before publishing its lease and
/// undo the charge on failure. Drop returns both storage and its account charge.
/// This is a contract only: no global allocator is installed by this crate.
pub trait ForwardingAllocator
{
    type Lease;
    fn allocate(&self, context: AllocationContext, layout: Layout)
                -> Result<Self::Lease, BudgetExceeded>;
}
