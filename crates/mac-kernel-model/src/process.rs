//! CPU-independent process/thread records. CPU frames are backend types and
//! vector storage is sized from a discovered architecture requirement.
use alloc::vec::Vec;

use crate::{DomainId, DomainKind, ResourceKey,
            pages::{AddressSpace, Architecture},
            policy::{AddressRange, ConfiguredRange, ModelError},
            zone::AllocationContext};

/// Separate identities matter: a definition executes/faults as its module,
/// resolves that module's resources, but allocates for the app's control zone.
#[derive(Clone, Copy, Debug)]
pub struct ExecutionBinding
{
    pub executor:   DomainId,
    pub subject:    DomainId,
    pub allocation: AllocationContext
}
/// Each entry is a ledger alias/pin, not a global resource-file pointer.
pub struct ResourceScopeStack
{
    scopes: Vec<ResourceKey>
}
impl ResourceScopeStack
{
    pub const fn empty() -> Self
    {
        Self { scopes: Vec::new() }
    }

    pub fn scopes(&self) -> &[ResourceKey]
    {
        &self.scopes
    }
    // RAII invocation guards and pins land with module dispatch, not a public
    // push/pop protocol applications could leave unbalanced.
}
/// Registered manager state, owned by the process ledger. The manager supplies
/// the kind-specific saved state; this record retains only its capability.
pub struct SavedManagerState
{
    owner: DomainId,
    slots: Vec<ResourceKey>
}
impl SavedManagerState
{
    pub const fn empty(owner: DomainId) -> Self
    {
        Self { owner,
               slots: Vec::new() }
    }

    pub fn register(&mut self, slot: ResourceKey) -> Result<(), ModelError>
    {
        if slot.owner() != self.owner
        {
            return Err(ModelError::WrongDomain);
        }
        if self.slots.iter().any(|s| s.kind() == slot.kind())
        {
            return Err(ModelError::InvalidBinding);
        }
        self.slots.try_reserve(1).map_err(|_| ModelError::MetadataAllocation)?;
        self.slots.push(slot);
        Ok(())
    }

    pub fn slots(&self) -> &[ResourceKey]
    {
        &self.slots
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorMarker
{
    NeverUsed,
    MayHoldState
}
/// No assumption about RV64's VLEN or x86's state layout. The adapter supplies
/// the *complete* save area requirement, including control state and alignment.
pub struct VectorStorage
{
    marker:   VectorMarker,
    required: core::alloc::Layout,
    backing:  Option<ResourceKey>
}
impl VectorStorage
{
    pub const fn lazy(required: core::alloc::Layout) -> Self
    {
        Self { marker: VectorMarker::NeverUsed,
               required,
               backing: None }
    }

    pub const fn marker(&self) -> VectorMarker
    {
        self.marker
    }

    pub const fn required(&self) -> core::alloc::Layout
    {
        self.required
    }

    pub const fn backing(&self) -> Option<ResourceKey>
    {
        self.backing
    }
}
pub struct StackReservation
{
    owner:  DomainId,
    range:  AddressRange,
    policy: ConfiguredRange
}
impl StackReservation
{
    pub fn new(owner: DomainId, range: AddressRange, policy: ConfiguredRange)
               -> Result<Self, ModelError>
    {
        if owner.kind() != DomainKind::Process || !policy.contains(range.bytes())
        {
            return Err(ModelError::InvalidBinding);
        }
        Ok(Self { owner,
                  range,
                  policy })
    }

    pub const fn owner(&self) -> DomainId
    {
        self.owner
    }

    pub const fn range(&self) -> AddressRange
    {
        self.range
    }

    pub const fn policy(&self) -> ConfiguredRange
    {
        self.policy
    }
}
/// Descriptor only. Trap entry/assembly will place the frame in this thread's
/// kernel domain *after* switching to the current CPU's System stack.
pub struct SavedFrame<A: Architecture>
{
    owner: DomainId,
    frame: A::TrapFrame
}
impl<A: Architecture> SavedFrame<A>
{
    pub fn new(owner: DomainId, frame: A::TrapFrame) -> Self
    {
        Self { owner,
               frame }
    }

    pub const fn owner(&self) -> DomainId
    {
        self.owner
    }

    pub fn frame(&self) -> &A::TrapFrame
    {
        &self.frame
    }
}
pub struct ThreadRecord<A: Architecture>
{
    identity: ResourceKey,
    stack:    StackReservation,
    saved:    SavedFrame<A>,
    vector:   VectorStorage
}
impl<A: Architecture> ThreadRecord<A>
{
    pub fn new(identity: ResourceKey, stack: StackReservation, saved: SavedFrame<A>,
               vector: VectorStorage)
               -> Result<Self, ModelError>
    {
        if identity.owner() != stack.owner || identity.owner() != saved.owner
        {
            return Err(ModelError::WrongDomain);
        }
        Ok(Self { identity,
                  stack,
                  saved,
                  vector })
    }

    pub const fn identity(&self) -> ResourceKey
    {
        self.identity
    }

    pub const fn stack(&self) -> &StackReservation
    {
        &self.stack
    }

    pub const fn saved(&self) -> &SavedFrame<A>
    {
        &self.saved
    }

    pub const fn vector(&self) -> &VectorStorage
    {
        &self.vector
    }
}
/// Accounts and the authoritative owner ledger live in DomainRegistry. These
/// references do not create another independent account or ID issuer.
pub struct ProcessRecord<'pool, 'storage, A: Architecture>
{
    domain:        DomainId,
    image:         ResourceKey,
    memory:        ResourceKey,
    legacy:        ResourceKey,
    address_space: AddressSpace<'pool, 'storage, A>,
    threads:       Vec<ThreadRecord<A>>,
    managers:      SavedManagerState,
    scopes:        ResourceScopeStack
}
impl<'pool, 'storage, A: Architecture> ProcessRecord<'pool, 'storage, A>
{
    pub fn new(domain: DomainId, image: ResourceKey, memory: ResourceKey, legacy: ResourceKey,
               address_space: AddressSpace<'pool, 'storage, A>)
               -> Result<Self, ModelError>
    {
        if domain.kind() != DomainKind::Process
           || domain != image.owner()
           || domain != memory.owner()
           || domain != legacy.owner()
           || domain != address_space.owner()
        {
            return Err(ModelError::WrongDomain);
        }
        Ok(Self { domain,
                  image,
                  memory,
                  legacy,
                  address_space,
                  threads: Vec::new(),
                  managers: SavedManagerState::empty(domain),
                  scopes: ResourceScopeStack::empty() })
    }

    pub const fn domain(&self) -> DomainId
    {
        self.domain
    }

    pub const fn image(&self) -> ResourceKey
    {
        self.image
    }

    pub const fn memory(&self) -> ResourceKey
    {
        self.memory
    }

    pub const fn legacy(&self) -> ResourceKey
    {
        self.legacy
    }

    pub fn may_hold_vector_state(&self) -> bool
    {
        self.threads.iter().any(|t| t.vector.marker == VectorMarker::MayHoldState)
    }

    pub fn register_manager(&mut self, slot: ResourceKey) -> Result<(), ModelError>
    {
        self.managers.register(slot)
    }

    pub fn add_thread(&mut self, thread: ThreadRecord<A>) -> Result<(), ModelError>
    {
        if thread.identity.owner() != self.domain
        {
            return Err(ModelError::WrongDomain);
        }
        if self.threads.iter().any(|t| t.identity == thread.identity)
        {
            return Err(ModelError::InvalidBinding);
        }
        self.threads.try_reserve(1).map_err(|_| ModelError::MetadataAllocation)?;
        self.threads.push(thread);
        Ok(())
    }

    pub fn address_space(&self) -> &AddressSpace<'pool, 'storage, A>
    {
        &self.address_space
    }

    pub fn threads(&self) -> &[ThreadRecord<A>]
    {
        &self.threads
    }

    pub const fn managers(&self) -> &SavedManagerState
    {
        &self.managers
    }

    pub const fn resource_scopes(&self) -> &ResourceScopeStack
    {
        &self.scopes
    }
}
