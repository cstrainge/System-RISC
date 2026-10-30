//! The one audited raw-RAM binding for the shared safe physical pool.
//! Firmware supplies permanent mapped views before pool owners exist. CPU and
//! physical addresses remain separate; references are never rebased or
//! transported through a physical/virtual mode flag.
use core::{alloc::Layout, fmt, marker::PhantomData, mem::MaybeUninit};

use mac_abi::boot::CheckedMemory;
use mac_kernel_model::{bootstrap::{BankPlan, Error as PlanError},
                       buddy::{BuddyError, BuddyPool, PageBlock, PageMetadata},
                       pages::{PageBox, PageGeometry, PhysicalAddress,
                               physical::{DirectMap, Error as MapError}},
                       policy::AddressRange};

use super::interrupts::NativeInvocationGate;
use crate::allocator::foundation::{self, Foundation};
type Pool = BuddyPool<'static, NativeInvocationGate>;
type Block = PageBlock<'static, 'static, NativeInvocationGate>;
mod application;
pub(crate) use application::{ApplicationExtentError, application_wait_extent,
                             prepare_application_extent};
struct Bank
{
    pool: Pool,
    next: Option<&'static Bank>
}
struct Pools
{
    first: Option<&'static Bank>,
    ram:   DirectMap<'static>
}
static POOLS: spin::Once<Pools> = spin::Once::new();
#[derive(Debug)]
pub(crate) enum Error
{
    Plan(PlanError),
    Pool(BuddyError),
    Unavailable,
    AlreadyInitialized,
    Exhausted
    {
        order:      u32,
        free_pages: usize
    },
    Geometry,
    ArrayLayout
    {
        elements:      usize,
        element_bytes: usize,
        alignment:     usize
    },
    Mapping(MapError),
    Model(mac_kernel_model::policy::ModelError)
}
impl Error
{
    pub(crate) fn status(&self) -> mac_abi::Status
    {
        match self
        {
            Self::Plan(PlanError::NoStorage { .. })
            | Self::Pool(BuddyError::Exhausted { .. })
            | Self::Exhausted { .. } => mac_abi::NO_MEMORY,
            Self::Unavailable => mac_abi::NOT_SUPPORTED,
            Self::AlreadyInitialized => mac_abi::BAD_ABI,
            Self::Plan(PlanError::Bank { .. } | PlanError::Layout { .. })
            | Self::Pool(BuddyError::Geometry(_)
                       | BuddyError::Metadata { .. }
                       | BuddyError::InvalidFreeRange
                       | BuddyError::AlreadyManaged)
            | Self::Geometry
            | Self::ArrayLayout { .. }
            | Self::Mapping(_)
            | Self::Model(_) => mac_abi::BAD_ARGUMENT
        }
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Plan(error) => error.fmt(f),
            Self::Pool(error) => error.fmt(f),
            Self::Unavailable => f.write_str("physical page pool has not been initialized"),
            Self::AlreadyInitialized =>
            {
                f.write_str("physical page pool is already initialized; reset before booting again")
            }
            Self::Exhausted { order,
                              free_pages } =>
            {
                write!(f,
                       "physical page allocation refused: order {order}, {free_pages} free pages \
                        across discovered RAM banks")
            }
            Self::Geometry =>
            {
                f.write_str("physical pool range or stack size exceeds the selected page geometry")
            }
            Self::ArrayLayout { elements,
                                element_bytes,
                                alignment } =>
            {
                write!(f,
                       "page-backed array layout refused: {elements} elements of {element_bytes} \
                        bytes with alignment {alignment}")
            }
            Self::Mapping(error) => error.fmt(f),
            Self::Model(error) => error.fmt(f)
        }
    }
}
/// Initialization keeps the exact element refusal, while the array retains and
/// drops every previously constructed element before returning its page lease.
#[derive(Debug)]
pub(crate) enum ArrayError<E>
{
    Storage(foundation::Error),
    Element
    {
        index: usize,
        cause: E
    }
}
impl<E: fmt::Display> fmt::Display for ArrayError<E>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Storage(error) => error.fmt(f),
            Self::Element { index,
                            cause } => write!(f, "page-backed array element {index}: {cause}")
        }
    }
}
/// # Safety
/// Single-hart bootstrap has exclusive access to every free extent in memory.
/// ROM exclusions include every flat image/definition arena, not just System.
/// Discovered RAM and metadata storage remain pinned with the same CPU mapping
/// until reset. This is called once before heap initialization or interrupts.
pub(crate) unsafe fn initialize(memory: CheckedMemory<'static>, ram: DirectMap<'static>)
                                -> Result<(), Error>
{
    if POOLS.get().is_some()
    {
        return Err(Error::AlreadyInitialized);
    }
    let geometry = PageGeometry::new(memory.page_bytes()).map_err(|_| Error::Geometry)?;
    // Preflight every bank before writing RAM. A later bank's ordinary refusal
    // cannot leave published partial pools, and no temporary Vec is needed.
    for index in 0..memory.banks().len()
    {
        BankPlan::new(memory, index, Layout::new::<Bank>()).map_err(Error::Plan)?;
    }
    let mut first = None;
    for index in 0..memory.banks().len()
    {
        let Some(plan) = BankPlan::new(memory, index, Layout::new::<Bank>()).map_err(Error::Plan)?
        else
        {
            continue;
        };
        let layout = plan.layout();
        let base = ram.virtual_address(PhysicalAddress(plan.storage().base), plan.storage().length)
                      .map_err(Error::Mapping)?
                      .0 as usize;
        // SAFETY: the checked layout reserves these disjoint aligned slices in
        // exclusively owned RAM. MaybeUninit permits the bytes' initial state;
        // the safe pool constructor writes valid values exactly once before
        // it creates PageMetadata/Option references.
        let (metadata, heads): (&'static mut [MaybeUninit<PageMetadata>],
                                &'static mut [MaybeUninit<Option<usize>>]) = unsafe {
            (core::slice::from_raw_parts_mut((base + layout.metadata_offset()) as *mut _,
                                             layout.pages()),
             core::slice::from_raw_parts_mut((base + layout.heads_offset()) as *mut _,
                                             layout.orders()))
        };
        let pool = Pool::initialize(PhysicalAddress(plan.bank().base), geometry, metadata, heads)
                        .map_err(Error::Pool)?;
        for free in plan.free_ranges()
        {
            pool.add_free_range(AddressRange::new(free.base, free.base + free.length)
                                .map_err(|_| Error::Geometry)?).map_err(Error::Pool)?;
        }
        let node = base as *mut Bank;
        // SAFETY: Bank occupies the checked control header, disjoint from its
        // metadata/heads and excluded from every free span. Written before
        // publication; the permanent reference is never mutable afterwards.
        unsafe {
            node.write(Bank { pool,
                              next: first });
            first = Some(&*node);
        }
        serial!("PAGE_BANK base={:#x} bytes={} metadata={:#x}+{} free_pages={}",
                plan.bank().base,
                plan.bank().length,
                plan.storage().base,
                plan.storage().length,
                unsafe { &*node }.pool.free_pages());
    }
    let pools = Pools { first,
                        ram };
    if pools.first.is_none()
    {
        return Err(Error::Exhausted { order:      0,
                                      free_pages: 0 });
    }
    POOLS.call_once(|| pools);
    Ok(())
}
fn allocate(order: u32) -> Result<Block, Error>
{
    let pools = POOLS.get().ok_or(Error::Unavailable)?;
    let mut next = pools.first;
    let mut free_pages = 0usize;
    while let Some(bank) = next
    {
        match bank.pool.allocate(order)
        {
            Ok(block) => return Ok(block),
            Err(BuddyError::Exhausted { free_pages: remaining,
                                        .. }) => free_pages = free_pages.saturating_add(remaining),
            Err(error) => return Err(Error::Pool(error))
        }
        next = bank.next;
    }
    Err(Error::Exhausted { order,
                           free_pages })
}
/// Exclusive RAM backing. CPU pointers are created only at this native binding;
/// initialization is selected explicitly, before creating any typed view.
pub(crate) struct Backing
{
    pages:    Block,
    cpu_base: usize
}
impl Backing
{
    /// Exact buddy extent used by allocation and by its caller's admission.
    /// The caller charges the complete physical block, including rounding.
    pub(crate) fn planned_bytes(bytes: usize) -> Result<u64, Error>
    {
        PageGeometry::new(super::memory::PAGE_BYTES).and_then(|geometry| {
                                                        geometry.buddy_bytes(bytes as u64)
                                                    })
                                                    .map_err(Error::Model)
    }

    pub(crate) fn allocate(bytes: usize) -> Result<Self, Error>
    {
        let backing = Self::reserve_uninitialized(bytes)?;
        // SAFETY: the unique lease owns the complete mapped extent. Existing
        // consumers retain their zero-initialized storage contract.
        unsafe {
            core::ptr::write_bytes(backing.cpu_base as *mut u8, 0, backing.bytes() as usize);
        }
        Ok(backing)
    }

    /// Reserve raw storage without touching its contents. Only allocator
    /// backing keeps this result uninitialized: GlobalAlloc returns raw storage and
    /// its free-list initializer writes its own metadata. Creating initialized
    /// Rust values or page tables still requires the caller's explicit writes.
    pub(crate) fn reserve_uninitialized(bytes: usize) -> Result<Self, Error>
    {
        let count = Self::planned_bytes(bytes)? / super::memory::PAGE_BYTES;
        let pages = allocate(count.ilog2())?;
        let cpu_base = POOLS.get()
                            .ok_or(Error::Unavailable)?
                            .ram
                            .virtual_address(pages.address(), pages.bytes())
                            .map_err(Error::Mapping)?
                            .0 as usize;
        Ok(Self { pages,
                  cpu_base })
    }

    pub(crate) fn cpu_base(&self) -> usize
    {
        self.cpu_base
    }

    pub(crate) fn physical(&self) -> PhysicalAddress
    {
        self.pages.address()
    }

    pub(crate) fn into_table(
        self)
        -> Result<PageBox<'static, 'static, [u64; 512], NativeInvocationGate>, Error>
    {
        PageBox::reserve(self.pages).map_err(Error::Model)
    }

    pub(crate) fn bytes(&self) -> u64
    {
        self.pages.bytes()
    }
}
/// Native typed metadata whose allocation precedes GlobalAlloc initialization.
/// The unique raw lease stays at its permanent physmap address when this owner
/// moves. Constructed elements are dropped before the underlying page block.
pub(crate) struct OwnedArray<T>
{
    backing:     Foundation<Backing>,
    initialized: usize,
    _values:     PhantomData<T>
}
impl<T> OwnedArray<T>
{
    pub(crate) fn try_new<E>(len: usize, mut initialize: impl FnMut(usize) -> Result<T, E>)
                             -> Result<Self, ArrayError<E>>
    {
        let invalid = || {
            foundation::Error::Pages(Error::ArrayLayout { elements:      len,
                                                          element_bytes: size_of::<T>(),
                                                          alignment:     align_of::<T>() })
        };
        let layout = Layout::array::<T>(len).map_err(|_| ArrayError::Storage(invalid()))?;
        if layout.size() == 0 || layout.align() > super::memory::PAGE_BYTES as usize
        {
            return Err(ArrayError::Storage(invalid()));
        }
        let backing = Foundation::<Backing>::reserve_uninitialized(layout.size())
            .map_err(ArrayError::Storage)?;
        if !backing.cpu_base().is_multiple_of(layout.align())
        {
            return Err(ArrayError::Storage(invalid()));
        }
        let mut array = Self { backing,
                               initialized: 0,
                               _values: PhantomData };
        for index in 0..len
        {
            let value = initialize(index).map_err(|cause| {
                                             ArrayError::Element { index,
                                                                   cause }
                                         })?;
            // SAFETY: Layout::array checked the complete span and alignment.
            // The unique lease contains no live value at this index yet, and
            // initialized advances only after that value has been written.
            unsafe {
                (array.backing.cpu_base() as *mut T).add(index).write(value);
            }
            array.initialized += 1;
        }
        Ok(array)
    }

    pub(crate) fn as_slice(&self) -> &[T]
    {
        // SAFETY: only the fully initialized array escapes try_new. Its unique
        // backing outlives this borrow, and mutable access requires &mut self.
        unsafe {
            core::slice::from_raw_parts(self.backing.cpu_base() as *const T, self.initialized)
        }
    }

    pub(crate) fn as_mut_slice(&mut self) -> &mut [T]
    {
        // SAFETY: the owner and this exclusive borrow cover all live elements.
        unsafe {
            core::slice::from_raw_parts_mut(self.backing.cpu_base() as *mut T, self.initialized)
        }
    }

    pub(crate) fn physical(&self) -> PhysicalAddress
    {
        self.backing.physical()
    }

    pub(crate) fn bytes(&self) -> u64
    {
        self.backing.bytes()
    }
}
impl<T> Drop for OwnedArray<T>
{
    fn drop(&mut self)
    {
        // SAFETY: initialized records exactly the live prefix, including when
        // an initializer refused. Backing remains owned throughout destruction
        // and its automatic field Drop returns the pages only afterwards.
        unsafe {
            core::ptr::drop_in_place(core::ptr::slice_from_raw_parts_mut(self.backing.cpu_base()
                                                                         as *mut T,
                                                                         self.initialized));
        }
    }
}
/// Native byte ownership for stack/CPU consumers. Protected pieces have no
/// buddy token; dropping them returns the actual boot-admitted Finder lease.
pub(crate) enum Storage
{
    Ordinary(Backing),
    Recovery(super::scheduler::recovery::FinderPiece)
}
impl Storage
{
    pub(crate) fn cpu_base(&self) -> usize
    {
        match self
        {
            Self::Ordinary(b) => b.cpu_base(),
            Self::Recovery(b) => b.cpu_base()
        }
    }

    pub(crate) fn physical(&self) -> PhysicalAddress
    {
        match self
        {
            Self::Ordinary(b) => b.physical(),
            Self::Recovery(b) => b.physical()
        }
    }

    pub(crate) fn bytes(&self) -> u64
    {
        match self
        {
            Self::Ordinary(b) => b.bytes(),
            Self::Recovery(b) => b.bytes()
        }
    }
}
/// Scheduler readers retain the enclosing ThreadCore. Stack and CPU backing
/// are disposed only after CPU/reader quiescence, through the same pool gate.
pub(crate) struct Stack
{
    backing:  Storage,
    bytes:    u64,
    cpu_base: usize
}
impl Stack
{
    pub(crate) fn allocate(bytes: usize) -> Result<Self, Error>
    {
        if !bytes.is_multiple_of(super::memory::PAGE_BYTES as usize)
        {
            return Err(Error::Geometry);
        }
        let backing = Backing::allocate(bytes)?;
        let cpu_base = backing.cpu_base();
        Ok(Self { backing: Storage::Ordinary(backing),
                  cpu_base,
                  bytes: bytes as u64 })
    }

    pub(crate) fn from_recovery(mut piece: super::scheduler::recovery::FinderPiece) -> Self
    {
        piece.activate();
        let bytes = piece.bytes();
        let cpu_base = piece.cpu_base();
        Self { backing: Storage::Recovery(piece),
               bytes,
               cpu_base }
    }

    /// # Safety
    /// Both owned permanent roots already map this stack's physical extent at
    /// cpu_base. The stack has never been published to an executor or used to
    /// form a pointer/reference into its storage; only its future numeric CPU
    /// address changes. The caller retains both roots and this lease until reset.
    pub(crate) unsafe fn bind_mapping(&mut self, cpu_base: usize) -> Result<(), Error>
    {
        if !cpu_base.is_multiple_of(super::memory::PAGE_BYTES as usize)
           || cpu_base.checked_add(self.bytes as usize).is_none()
        {
            return Err(Error::Geometry);
        }
        self.cpu_base = cpu_base;
        Ok(())
    }

    pub(crate) fn physical(&self) -> PhysicalAddress
    {
        self.backing.physical()
    }

    pub(crate) fn bytes(&self) -> u64
    {
        self.bytes
    }

    pub(crate) fn backing_bytes(&self) -> u64
    {
        self.backing.bytes()
    }

    pub(crate) fn top(&self) -> usize
    {
        self.cpu_base + self.bytes as usize
    }
}

/// Backend counts only; physmap aliases do not add physical ownership.
pub(crate) fn free_pages() -> Result<u64, Error>
{
    let pools = POOLS.get().ok_or(Error::Unavailable)?;
    let mut bank = pools.first;
    let mut total = 0u64;
    while let Some(current) = bank
    {
        total = total.checked_add(current.pool.free_pages() as u64).ok_or(Error::Geometry)?;
        bank = current.next;
    }
    Ok(total)
}
