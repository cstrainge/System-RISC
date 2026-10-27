//! Page geometry, ownership and mapping plans; no PTE bits or CPU instructions.
use alloc::vec::Vec;
use core::marker::PhantomData;

use mac_object_table::shared::InvocationGate;
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
#[derive(Clone, Copy, Debug)]
pub struct Permissions
{
    pub read:    bool,
    pub write:   bool,
    pub execute: bool,
    pub access:  Access,
    pub global:  bool
}
/// Backend-specific encodings, CPU context and address checks stay outside the
/// common model. Dispatch here is compile-time, not a dyn call per mapped page.
pub trait Architecture
{
    type InvocationGate: InvocationGate;
    type PageTable;
    type TrapFrame;
    fn geometry() -> PageGeometry;
    fn leaf_sizes() -> &'static [u64];
    fn valid_virtual_range(range: AddressRange) -> bool;
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
/// A typed physical-page reservation, not an initialized Rust T. There is no
/// Deref and no global virtual/physical-mode flag. A future audited mapper
/// supplies initialized mapped access before a root can be activated.
pub struct PageBox<'pool, 'storage, T, G: InvocationGate>
{
    pub(crate) page: PageBlock<'pool, 'storage, G>,
    marker:          PhantomData<T>
}
impl<'pool, 'storage, T, G: InvocationGate> PageBox<'pool, 'storage, T, G>
{
    pub fn reserve(page: PageBlock<'pool, 'storage, G>) -> Result<Self, ModelError>
    {
        if page.order() != 0
           || size_of::<T>() as u64 > page.bytes()
           || align_of::<T>() as u64 > page.bytes()
        {
            return Err(ModelError::InvalidBinding);
        }
        Ok(Self { page,
                  marker: PhantomData })
    }

    pub fn physical(&self) -> PhysicalAddress
    {
        self.page.address()
    }
}
#[derive(Clone, Copy, Debug)]
pub struct AsidTag
{
    pub hardware_value: u64,
    pub generation:     core::num::NonZeroU64
}
pub struct AddressSpace<'pool, 'storage, A: Architecture>
{
    owner: DomainId,
    root:  PageBox<'pool, 'storage, A::PageTable, A::InvocationGate>,
    asid:  AsidTag,
    state: Spinlock<AddressSpaceState>
}
struct AddressSpaceState
{
    mappings:               Vec<Mapping>,
    outstanding_shootdowns: usize
}
impl<'pool, 'storage, A: Architecture> AddressSpace<'pool, 'storage, A>
{
    /// A reserved, uninitialized root. This shape cannot activate a CPU page
    /// table; initialized mapper/activation proof is a later platform boundary.
    pub fn reserve(owner: DomainId,
                   root: PageBox<'pool, 'storage, A::PageTable, A::InvocationGate>, asid: AsidTag)
                   -> Result<Self, ModelError>
    {
        if root.page.bytes() != A::geometry().bytes()
        {
            return Err(ModelError::InvalidGeometry);
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
