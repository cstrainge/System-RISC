//! Page geometry, ownership and mapping plans; no PTE bits or CPU instructions.
pub mod mapping;
pub mod walk;
pub mod physical;
use alloc::vec::Vec;
use core::marker::PhantomData;

pub use mac_object_table::shared::InvocationGate;
use spinning_top::Spinlock;

use crate::{DomainId,
            buddy::PageBlock,
            policy::{AddressRange, ModelError}};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageGeometry
{
    bytes: u64
}
impl PageGeometry
{
    pub const fn new(bytes: u64) -> Result<Self, ModelError>
    {
        if bytes.is_power_of_two() { Ok(Self { bytes }) } else { Err(ModelError::InvalidGeometry) }
    }

    pub const fn bytes(self) -> u64
    {
        self.bytes
    }

    pub fn pages(self, bytes: u64) -> u64
    {
        bytes.div_ceil(self.bytes)
    }

    /// Complete physical extent of one nonempty buddy allocation. Admission,
    /// allocation and build-time quota checks share this rounding contract.
    pub fn buddy_bytes(self, bytes: u64) -> Result<u64, ModelError>
    {
        if bytes == 0
        {
            return Err(ModelError::InvalidGeometry);
        }
        self.pages(bytes)
            .checked_next_power_of_two()
            .and_then(|pages| pages.checked_mul(self.bytes))
            .ok_or(ModelError::Overflow)
    }

    pub const fn aligned(self, address: u64) -> bool
    {
        address.is_multiple_of(self.bytes)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysicalAddress(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VirtualAddress(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access
{
    Supervisor,
    User
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageOwnership
{
    Manual,
    Automatic,
    CopyOnWrite
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Permissions
{
    pub read:    bool,
    pub write:   bool,
    pub execute: bool,
    pub access:  Access,
    pub global:  bool
}
/// Pure geometry/address checks shared by build tooling and firmware; no fake
/// CPU frame or invocation gate is needed just to validate a layout.
/// One encoded page-table entry, as the mapping machinery needs it.
///
/// This is the neutral view: what an entry *means*, expressed in bytes and
/// permissions rather than in one format's enumerations. Each architecture
/// implements it over its own descriptor type, so the walk and the mapping plan
/// are written once and neither names an ISA.
pub trait Descriptor: Copy + core::fmt::Debug + PartialEq + Eq
{
    type Error: core::fmt::Display;

    /// An entry that maps nothing.
    fn invalid() -> Self;

    /// The raw bits, as stored in a table slot.
    fn bits(self) -> u64;

    /// A pointer to the next table level.
    fn table(address: PhysicalAddress, global: bool, ownership: PageOwnership)
             -> Result<Self, Self::Error>;

    /// A leaf mapping `size_bytes`, which must be one of the format's leaves.
    fn leaf(address: PhysicalAddress, size_bytes: u64, permissions: Permissions,
            ownership: PageOwnership)
            -> Result<Self, Self::Error>;

    /// What `bits` means at `level`, counted from the finest leaf.
    fn decode(bits: u64, level: u8) -> Result<EntryKind, Self::Error>;
}

/// The neutral meaning of an encoded entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind
{
    Invalid,
    Table
    {
        physical:  PhysicalAddress,
        global:    bool,
        ownership: PageOwnership
    },
    Leaf
    {
        physical:    PhysicalAddress,
        size_bytes:  u64,
        permissions: Permissions,
        ownership:   PageOwnership
    }
}

pub trait PageLayout
{
    fn geometry() -> PageGeometry;
    fn leaf_sizes() -> &'static [u64];
    fn valid_virtual_range(range: AddressRange) -> bool;

    /// The table index one address selects at a given leaf size.
    ///
    /// This is geometry, not encoding: a page-table walk of any 39-bit,
    /// three-level format picks an index by shifting the address down to the
    /// leaf's own size and taking the low nine bits. Keying on the leaf size
    /// rather than on a level number is what lets both supported architectures
    /// share it, because they number their levels differently — Sv39 counts the
    /// 4 KiB level 0 and AArch64 counts it 3.
    fn index(address: u64, leaf_bytes: u64) -> Option<usize>
    {
        if !leaf_bytes.is_power_of_two() || leaf_bytes < Self::geometry().bytes()
        {
            return None;
        }
        if !Self::leaf_sizes().contains(&leaf_bytes)
        {
            return None;
        }
        let shift = leaf_bytes.trailing_zeros();
        Some(((address >> shift) & 511) as usize)
    }

    /// The number of levels a walk has, derived from the leaves this format
    /// declares rather than stated twice.
    fn levels() -> usize
    {
        Self::leaf_sizes().len()
    }

    /// The first address above the lower canonical half. An extent ending here
    /// has exhausted the lower half, not the address space.
    fn lower_half_end() -> u64;

    /// The first address of the upper canonical half.
    fn upper_half_start() -> u64;

    /// Whether one address is canonical for this format.
    fn canonical(address: u64) -> bool
    {
        let Some(end) = address.checked_add(1)
        else
        {
            return false;
        };
        match crate::policy::AddressRange::new(address, end)
        {
            Ok(range) => Self::valid_virtual_range(range),
            Err(_) => false
        }
    }

    /// The leaf size used at `level`, counting from the finest leaf.
    ///
    /// Derived from the page geometry and the nine address bits each level
    /// consumes, not from the order a format happens to list its leaves in:
    /// `leaf_sizes` is written largest-first by some formats and this
    /// convention is finest-first, so indexing it directly would silently pick
    /// the wrong leaf.
    fn leaf_at(level: usize) -> Option<u64>
    {
        if level >= Self::levels()
        {
            return None;
        }
        let base = Self::geometry().bytes();
        Some(base << (9 * level as u32))
    }
}
/// CPU context and its actual invocation gate belong to the resident binding.
/// Dispatch remains compile-time, not a dyn call per mapped page.
pub trait Architecture: PageLayout
{
    type InvocationGate: InvocationGate;
    type PageTable;
    type TrapFrame;
}
pub struct Mapping
{
    pub physical:        PhysicalAddress,
    pub virtual_address: VirtualAddress,
    pub bytes:           u64
}
/// Produces one entry per leaf, not one operation per base page of RAM.
pub struct MappingPlan<'a>
{
    physical:        u64,
    virtual_address: u64,
    remaining:       u64,
    leaves:          &'a [u64]
}
impl<'a> MappingPlan<'a>
{
    pub fn new(physical: PhysicalAddress, virtual_address: VirtualAddress, bytes: u64,
               geometry: PageGeometry, leaves: &'a [u64])
               -> Result<Self, ModelError>
    {
        if leaves.is_empty()
           || leaves.last() != Some(&geometry.bytes())
           || leaves.iter().any(|n| !n.is_power_of_two() || !n.is_multiple_of(geometry.bytes()))
           || leaves.windows(2).any(|p| p[0] <= p[1])
           || !geometry.aligned(physical.0)
           || !geometry.aligned(virtual_address.0)
           || !geometry.aligned(bytes)
        {
            return Err(ModelError::InvalidGeometry);
        }
        physical.0.checked_add(bytes).ok_or(ModelError::Overflow)?;
        virtual_address.0.checked_add(bytes).ok_or(ModelError::Overflow)?;
        Ok(Self { physical: physical.0,
                  virtual_address: virtual_address.0,
                  remaining: bytes,
                  leaves })
    }
}
impl Iterator for MappingPlan<'_>
{
    type Item = Mapping;

    fn next(&mut self) -> Option<Mapping>
    {
        if self.remaining == 0
        {
            return None;
        }
        let bytes = *self.leaves.iter().find(|&&n| {
                                            n <= self.remaining
                                            && self.physical.is_multiple_of(n)
                                            && self.virtual_address.is_multiple_of(n)
                                        })?;
        let mapping = Mapping { physical: PhysicalAddress(self.physical),
                                virtual_address: VirtualAddress(self.virtual_address),
                                bytes };
        self.physical += bytes;
        self.virtual_address += bytes;
        self.remaining -= bytes;
        Some(mapping)
    }
}
/// Metadata exposed by an owned physical reservation. This safe trait does not
/// authenticate RAM, grant mapped access, or authorize CPU activation. Those
/// guarantees belong to the native binding's private backing constructors and
/// activation path, not to arbitrary implementations of this metadata contract.
/// The reported address and extent must remain stable while this backing is
/// retained; the native binding enforces that through immutable lease metadata.
pub trait PageBacking
{
    fn physical(&self) -> PhysicalAddress;
    fn bytes(&self) -> u64;
}
impl<G: InvocationGate> PageBacking for PageBlock<'_, '_, G>
{
    fn physical(&self) -> PhysicalAddress
    {
        self.address()
    }

    fn bytes(&self) -> u64
    {
        PageBlock::bytes(self)
    }
}
/// Owns one page reservation, not an initialized Rust T. There is no Deref,
/// mapped access or CPU activation. Native users must supply genuine backing
/// and initialize it through their audited mapper before activating a root.
pub struct PageBox<T, B: PageBacking>
{
    backing: B,
    marker:  PhantomData<T>
}
impl<T, B: PageBacking> PageBox<T, B>
{
    pub fn reserve(backing: B, geometry: PageGeometry) -> Result<Self, ModelError>
    {
        let bytes = geometry.bytes();
        let physical = backing.physical();
        let extent = backing.bytes();
        if extent != bytes
           || !physical.0.is_multiple_of(bytes)
           || physical.0.checked_add(bytes).is_none()
           || size_of::<T>() as u64 > bytes
           || align_of::<T>() as u64 > bytes
        {
            return Err(ModelError::InvalidBinding);
        }
        Ok(Self { backing,
                  marker: PhantomData })
    }

    pub fn physical(&self) -> PhysicalAddress
    {
        self.backing.physical()
    }
}
#[derive(Clone, Copy, Debug)]
pub enum AsidTag
{
    /// No cache identity is retained across root changes. The native binding
    /// must perform a full local translation invalidation on activation.
    Untagged,
    /// Reset-lifetime System roots use permanently reserved hardware cache
    /// tags, not fabricated object generations or application admission slots.
    Permanent
    {
        hardware_value: u64
    },
    Leased
    {
        hardware_value: u64, generation: core::num::NonZeroU64
    }
}
pub struct AddressSpace<A: Architecture, B: PageBacking>
{
    owner: DomainId,
    root:  PageBox<A::PageTable, B>,
    asid:  AsidTag,
    state: Spinlock<AddressSpaceState>
}
struct AddressSpaceState
{
    mappings:               Vec<Mapping>,
    outstanding_shootdowns: usize
}
impl<A: Architecture, B: PageBacking> AddressSpace<A, B>
{
    /// A reserved, uninitialized root. This shape cannot activate a CPU page
    /// table; initialized mapper/activation proof is a later platform boundary.
    pub fn reserve(owner: DomainId, root: PageBox<A::PageTable, B>, asid: AsidTag)
                   -> Result<Self, ModelError>
    {
        if root.backing.bytes() != A::geometry().bytes()
        {
            return Err(ModelError::InvalidGeometry);
        }
        if matches!(asid, AsidTag::Permanent { .. }) && owner != DomainId::SYSTEM
        {
            return Err(ModelError::WrongDomain);
        }
        Ok(Self { owner,
                  root,
                  asid,
                  state: Spinlock::new(AddressSpaceState { mappings:               Vec::new(),
                                                           outstanding_shootdowns: 0 }) })
    }

    pub const fn owner(&self) -> DomainId
    {
        self.owner
    }

    pub fn root_physical(&self) -> PhysicalAddress
    {
        self.root.physical()
    }

    pub const fn asid(&self) -> AsidTag
    {
        self.asid
    }

    pub fn pending_shootdowns(&self) -> usize
    {
        A::InvocationGate::run(|| self.state.lock().outstanding_shootdowns)
    }

    pub fn planned_mappings(&self) -> usize
    {
        A::InvocationGate::run(|| self.state.lock().mappings.len())
    }
}
