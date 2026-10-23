//! Reviewed ranges, uniform layout input and two-level accounting records.
use alloc::vec::Vec;
use core::fmt;

use mac_object_table::domain::OwnerIndex;

use crate::DomainId;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelError
{
    InvalidRange,
    Overflow,
    InvalidGeometry,
    InvalidReservation,
    MetadataAllocation,
    InvalidLayout,
    InvalidReserve,
    InvalidPercentage,
    InvalidManifest,
    InvalidAddressRange,
    WrongDomain,
    InvalidBinding
}
impl fmt::Display for ModelError
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        f.write_str(match self
                    {
                        Self::InvalidRange =>
                        {
                            "invalid configured range: require minimum <= default <= maximum"
                        }
                        Self::Overflow => "memory size or address arithmetic overflow",
                        Self::InvalidGeometry => "invalid platform page geometry or mapping sizes",
                        Self::InvalidReservation =>
                        {
                            "boot reservation is overlapping, unaligned, or outside discovered RAM"
                        }
                        Self::MetadataAllocation =>
                        {
                            "backing allocation refused for memory-model metadata"
                        }
                        Self::WrongDomain =>
                        {
                            "memory-model object belongs to a different lifetime domain"
                        }
                        Self::InvalidBinding => "invalid or duplicate object lifetime binding",
                        Self::InvalidLayout =>
                        {
                            "uniform virtual ranges overlap, are out of order, exclude the \
                             physmap, or exclude the image base"
                        }
                        Self::InvalidReserve =>
                        {
                            "System recovery floor exceeds its heap, or Finder restart space \
                             exceeds the protected floor"
                        }
                        Self::InvalidPercentage =>
                        {
                            "owned text storage percentage must be between 0 and 100"
                        }
                        Self::InvalidManifest =>
                        {
                            "emergency manifest has a duplicate service or non-power-of-two \
                             alignment"
                        }
                        Self::InvalidAddressRange =>
                        {
                            "address range must be nonempty with start below end"
                        }
                    })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfiguredRange
{
    minimum: u64,
    default: u64,
    maximum: u64
}
impl ConfiguredRange
{
    pub const fn new(minimum: u64, default: u64, maximum: u64) -> Result<Self, ModelError>
    {
        if minimum <= default && default <= maximum
        {
            Ok(Self { minimum,
                      default,
                      maximum })
        }
        else
        {
            Err(ModelError::InvalidRange)
        }
    }

    pub const fn minimum(self) -> u64
    {
        self.minimum
    }

    pub const fn default_value(self) -> u64
    {
        self.default
    }

    pub const fn maximum(self) -> u64
    {
        self.maximum
    }

    pub const fn contains(self, n: u64) -> bool
    {
        n >= self.minimum && n <= self.maximum
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddressRange
{
    start: u64,
    end:   u64
}
impl AddressRange
{
    pub const fn new(start: u64, end: u64) -> Result<Self, ModelError>
    {
        if start < end
        {
            Ok(Self { start,
                      end })
        }
        else
        {
            Err(ModelError::InvalidAddressRange)
        }
    }

    pub const fn start(self) -> u64
    {
        self.start
    }

    pub const fn end(self) -> u64
    {
        self.end
    }

    pub const fn bytes(self) -> u64
    {
        self.end - self.start
    }

    pub const fn contains(self, r: Self) -> bool
    {
        r.start >= self.start && r.end <= self.end
    }

    pub const fn overlaps(self, r: Self) -> bool
    {
        self.start < r.end && r.start < self.end
    }
}
/// Deliberately no Default: addresses and budgets await the reviewed profile.
/// This record is the common input for Rust configuration and linker emission;
/// it is not a second set of architecture-specific linker constants.
pub struct UniformLayout
{
    application:    AddressRange,
    process_kernel: AddressRange,
    modules:        AddressRange,
    kernel:         AddressRange,
    physmap:        AddressRange,
    image_base:     u64
}
impl UniformLayout
{
    pub fn new(application: AddressRange, process_kernel: AddressRange, modules: AddressRange,
               kernel: AddressRange, physmap: AddressRange, image_base: u64)
               -> Result<Self, ModelError>
    {
        if application.end() > process_kernel.start()
           || process_kernel.end() > modules.start()
           || modules.end() > kernel.start()
           || !kernel.contains(physmap)
           || !(application.start()..application.end()).contains(&image_base)
        {
            return Err(ModelError::InvalidLayout);
        }
        Ok(Self { application,
                  process_kernel,
                  modules,
                  kernel,
                  physmap,
                  image_base })
    }

    pub const fn application(&self) -> AddressRange
    {
        self.application
    }

    pub const fn process_kernel(&self) -> AddressRange
    {
        self.process_kernel
    }

    pub const fn modules(&self) -> AddressRange
    {
        self.modules
    }

    pub const fn kernel(&self) -> AddressRange
    {
        self.kernel
    }

    pub const fn physmap(&self) -> AddressRange
    {
        self.physmap
    }

    pub const fn image_base(&self) -> u64
    {
        self.image_base
    }

    pub fn validate_architecture<A: crate::pages::Architecture>(&self) -> Result<(), ModelError>
    {
        let geometry = A::geometry();
        if [self.application,self.process_kernel,self.modules,self.kernel,self.physmap].iter()
            .any(|r|!A::valid_virtual_range(*r)||!geometry.aligned(r.start())||!geometry.aligned(r.end()))
            || !geometry.aligned(self.image_base) {return Err(ModelError::InvalidGeometry);}
        Ok(())
    }

    /// The platform validates canonical addresses/alignment before emitting this.
    pub fn linker_symbols(&self, out: &mut impl fmt::Write) -> fmt::Result
    {
        for (name, range) in [("APPLICATION", self.application),
                              ("PROCESS_KERNEL", self.process_kernel),
                              ("MODULE", self.modules),
                              ("KERNEL", self.kernel),
                              ("PHYSMAP", self.physmap)]
        {
            writeln!(out, "__{name}_START = {:#x};", range.start())?;
            writeln!(out, "__{name}_END = {:#x};", range.end())?;
        }
        writeln!(out, "__APPLICATION_IMAGE_BASE = {:#x};", self.image_base)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryClass
{
    System,
    Kernel,
    Application,
    Control
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetExceeded
{
    pub owner:     DomainId,
    pub class:     MemoryClass,
    pub used:      u64,
    pub limit:     u64,
    pub requested: u64,
    pub reason:    BudgetReason
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetReason
{
    DomainLimit,
    SystemRecoveryFloor,
    PhysicalMemory,
    AccountingOverflow
}
impl fmt::Display for BudgetExceeded
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        write!(f,
               "{:?} memory refused for {}: {} bytes used, {} byte limit, {} bytes requested \
                ({:?})",
               self.class, self.owner, self.used, self.limit, self.requested, self.reason)
    }
}
/// The same account covers service metadata and its subzones. Charging and
/// lease release will be implemented with the live allocator, not simulated.
pub struct DomainAccount
{
    owner:       DomainId,
    kernel:      AccountUsage,
    application: AccountUsage
}
impl DomainAccount
{
    pub fn new(owner: DomainId, kernel: ConfiguredRange, application: ConfiguredRange) -> Self
    {
        Self { owner,
               kernel: AccountUsage::new(kernel),
               application: AccountUsage::new(application) }
    }

    pub const fn owner(&self) -> DomainId
    {
        self.owner
    }

    pub const fn kernel(&self) -> &AccountUsage
    {
        &self.kernel
    }

    pub const fn application(&self) -> &AccountUsage
    {
        &self.application
    }
}
#[derive(Clone, Copy, Debug)]
pub struct AccountUsage
{
    committed: u64,
    reserved:  u64,
    limit:     ConfiguredRange
}
impl AccountUsage
{
    const fn new(limit: ConfiguredRange) -> Self
    {
        Self { committed: 0,
               reserved: 0,
               limit }
    }

    pub const fn committed(self) -> u64
    {
        self.committed
    }

    pub const fn reserved(self) -> u64
    {
        self.reserved
    }

    pub const fn limit(self) -> ConfiguredRange
    {
        self.limit
    }
}
pub struct SystemReserve
{
    heap:           ConfiguredRange,
    recovery_floor: u64,
    finder_restart: u64
}
impl SystemReserve
{
    /// Finder restart space is a named part of the protected floor, not an
    /// additional allocation from the process-chargeable remainder.
    pub fn new(heap: ConfiguredRange, recovery_floor: u64, finder_restart: u64)
               -> Result<Self, ModelError>
    {
        if recovery_floor > heap.default_value() || finder_restart > recovery_floor
        {
            return Err(ModelError::InvalidReserve);
        }
        Ok(Self { heap,
                  recovery_floor,
                  finder_restart })
    }

    pub const fn heap(&self) -> ConfiguredRange
    {
        self.heap
    }

    pub const fn recovery_floor(&self) -> u64
    {
        self.recovery_floor
    }

    pub const fn finder_restart(&self) -> u64
    {
        self.finder_restart
    }
}
pub struct DomainRegistry
{
    domains: Vec<DomainRecord>,
    system:  SystemReserve
}
impl DomainRegistry
{
    pub fn new(system: SystemReserve) -> Self
    {
        Self { domains: Vec::new(),
               system }
    }

    pub const fn system(&self) -> &SystemReserve
    {
        &self.system
    }

    pub fn domains(&self) -> impl Iterator<Item = &DomainRecord>
    {
        self.domains.iter()
    }

    pub fn insert(&mut self, record: DomainRecord) -> Result<(), ModelError>
    {
        if self.domains.iter().any(|d| d.identity() == record.identity())
        {
            return Err(ModelError::InvalidBinding);
        }
        self.domains.try_reserve(1).map_err(|_| ModelError::MetadataAllocation)?;
        self.domains.push(record);
        Ok(())
    }
}
pub struct DomainRecord
{
    ledger:  OwnerIndex,
    account: DomainAccount,
    state:   DomainState
}
impl DomainRecord
{
    pub fn new(ledger: OwnerIndex, kernel: ConfiguredRange, application: ConfiguredRange) -> Self
    {
        Self { account: DomainAccount::new(ledger.owner(), kernel, application),
               ledger,
               state: DomainState::Constructing }
    }

    pub fn identity(&self) -> DomainId
    {
        self.ledger.owner()
    }

    pub const fn account(&self) -> &DomainAccount
    {
        &self.account
    }

    pub const fn ledger(&self) -> &OwnerIndex
    {
        &self.ledger
    }

    pub const fn state(&self) -> DomainState
    {
        self.state
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomainState
{
    Constructing,
    Live,
    Stopping,
    Reaped
}
/// An observation, not permission to reclaim pages. Live integration must stop
/// admission and obtain actual scheduler/I/O quiescence before freeing memory.
pub struct DeathBarrierObservation
{
    pub live_handles:    usize,
    pub running_threads: usize,
    pub pending_work:    usize
}
impl DeathBarrierObservation
{
    pub fn drained(&self) -> bool
    {
        self.live_handles == 0 && self.running_threads == 0 && self.pending_work == 0
    }
}
