//! The one audited raw-RAM binding for the shared safe physical pool.
//! Current execution is flat M mode. When the architecture handoff lands, this
//! initialization must run after permanent mapped views exist; references are
//! never rebased or transported through a physical/virtual mode flag.
use core::{alloc::Layout, fmt, mem::MaybeUninit};

use mac_abi::boot::CheckedMemory;
use mac_kernel_model::{bootstrap::{BankPlan, Error as PlanError},
                       buddy::{BuddyError, BuddyPool, PageBlock, PageMetadata},
                       pages::{PageGeometry, PhysicalAddress},
                       policy::AddressRange};

use super::interrupts::NativeInvocationGate;
type Pool = BuddyPool<'static, NativeInvocationGate>;
type Block = PageBlock<'static, 'static, NativeInvocationGate>;
struct Bank
{
    pool: Pool,
    next: Option<&'static Bank>
}
struct Pools
{
    first: Option<&'static Bank>
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
    Geometry
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
            | Self::Geometry => mac_abi::BAD_ARGUMENT
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
        }
    }
}
/// # Safety
/// Single-hart bootstrap has exclusive access to every free extent in memory.
/// ROM exclusions include every flat image/definition arena, not just System.
/// Discovered RAM and metadata storage remain pinned with the same CPU mapping
/// until reset. This is called once before heap initialization or interrupts.
pub(crate) unsafe fn initialize(memory: CheckedMemory<'_>) -> Result<(), Error>
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
        let base = plan.storage().base as usize;
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
    let pools = Pools { first };
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
/// Exclusive zeroed RAM backing. CPU pointers are created only at this native
/// binding; flat execution is replaced with stable mapped views at MMU handoff.
pub(crate) struct Backing
{
    pages: Block
}
impl Backing
{
    pub(crate) fn allocate(bytes: usize) -> Result<Self, Error>
    {
        let page = super::memory::PAGE_BYTES as usize;
        let count = bytes.checked_add(page - 1)
                         .filter(|_| bytes != 0)
                         .map(|n| n / page)
                         .and_then(usize::checked_next_power_of_two)
                         .ok_or(Error::Geometry)?;
        let pages = allocate(count.ilog2())?;
        // SAFETY: exclusive page lease, flat physical mapping, all bytes are
        // ordinary discovered RAM. Clear prior data before any owner sees it.
        unsafe {
            core::ptr::write_bytes(pages.address().0 as *mut u8, 0, pages.bytes() as usize);
        }
        Ok(Self { pages })
    }

    pub(crate) fn cpu_base(&self) -> usize
    {
        self.pages.address().0 as usize
    }

    pub(crate) fn bytes(&self) -> u64
    {
        self.pages.bytes()
    }
}
/// Scheduler readers retain the enclosing ThreadCore. Stack and CPU backing
/// are disposed only after CPU/reader quiescence, through the same pool gate.
pub(crate) struct Stack
{
    backing: Backing,
    bytes:   u64
}
impl Stack
{
    pub(crate) fn allocate(bytes: usize) -> Result<Self, Error>
    {
        if !bytes.is_multiple_of(super::memory::PAGE_BYTES as usize)
        {
            return Err(Error::Geometry);
        }
        Ok(Self { backing: Backing::allocate(bytes)?,
                  bytes:   bytes as u64 })
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
        self.backing.cpu_base() + self.bytes as usize
    }
}
