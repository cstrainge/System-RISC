//! Resident allocation boundary. Every block retains its original admission,
//! including its header and the free-list backend's alignment requirements.
//! Account selection and refund never run with the free-list lock held.
use core::{alloc::{GlobalAlloc, Layout},
           fmt,
           ptr::{self, NonNull}};

use linked_list_allocator::{LockedHeap, hole::HoleList};
use mac_kernel_model::{account::{Committed, Reserved},
                       policy::MemoryClass};

use crate::machine::{interrupts::{NativeInvocationGate, with_masked},
                     scheduler::accounting::AllocationAuthority};

mod budget;
pub(crate) mod recovery;

pub(crate) fn statistics() -> budget::Snapshot
{
    budget::snapshot()
}
pub(crate) const HEADER_BYTES: usize = core::mem::size_of::<Header>();

use mac_object_table::scope::AllocationAdmission;

use crate::allocation::OperationTarget;
pub(crate) use crate::allocation::{SystemClass, Target};

#[derive(Clone, Copy)]
pub(crate) enum Selection<'a>
{
    System(SystemClass),
    Operation(&'a OperationTarget),
    Owner
    {
        account: &'a AllocationAuthority,
        class:   MemoryClass
    }
}
impl Target
{
    pub(crate) fn selection(&self) -> Selection<'_>
    {
        match self
        {
            Self::System(class) => Selection::System(*class),
            Self::Operation(operation) => Selection::Operation(operation),
            Self::Owner { account,
                          class } =>
            {
                Selection::Owner { account,
                                   class: *class }
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum Error
{
    Account(mac_kernel_model::account::Error),
    Operation(mac_object_table::scope::Failure),
    Layout,
    SystemLimit
    {
        used:      u64,
        limit:     u64,
        requested: u64
    },
    ContextUnavailable,
    Recovery(recovery::Error)
}
impl fmt::Display for Error
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Account(error) => error.fmt(out),
            Self::Operation(error) => error.fmt(out),
            Self::Layout => out.write_str("resident allocation layout or memory class is invalid"),
            Self::SystemLimit { used,
                                limit,
                                requested } =>
            {
                write!(out,
                       "ordinary System heap refused {requested} bytes: {used} bytes admitted, \
                        {limit} byte capacity; protected recovery storage is excluded")
            }
            Self::Recovery(error) => error.fmt(out),
            Self::ContextUnavailable =>
            {
                out.write_str("scheduled execution has no retained allocation authority")
            }
        }
    }
}

pub(crate) fn with_system<R>(class: SystemClass, operation: impl FnOnce() -> R) -> R
{
    crate::context::with_system_allocation(class, operation)
}
pub(crate) fn with_target<R>(target: Target, operation: impl FnOnce() -> R) -> R
{
    crate::context::with_allocation(target, operation)
}

/// Backing origin and its occupancy receipt are one value. Account attribution
/// remains independent: protected storage never augments ordinary capacity or
/// retargets a block when a different execution scope later disposes it.
enum ParentReceipt
{
    Ordinary(budget::Receipt),
    Protected(recovery::Receipt)
}
impl ParentReceipt
{
    fn reserve(category: budget::Category, bytes: u64, recovery: Option<&recovery::Permit>)
               -> Result<Self, Error>
    {
        match recovery
        {
            Some(permit) =>
            {
                permit.reserve(category, bytes).map(Self::Protected).map_err(Error::Recovery)
            }
            None => budget::reserve(category, bytes).map(Self::Ordinary)
        }
    }

    fn reserve_replacement(&self, bytes: u64) -> Result<Self, Error>
    {
        match self
        {
            Self::Ordinary(receipt) =>
            {
                budget::reserve(receipt.category(), bytes).map(Self::Ordinary)
            }
            Self::Protected(receipt) =>
            {
                receipt.reserve_replacement(bytes).map(Self::Protected).map_err(Error::Recovery)
            }
        }
    }

    fn allocate(&self, ordinary: &LockedHeap, layout: Layout) -> Result<NonNull<u8>, ()>
    {
        match self
        {
            Self::Ordinary(_) => ordinary.lock().allocate_first_fit(layout).map_err(|_| ()),
            Self::Protected(receipt) => receipt.allocate(layout).map_err(|_| ())
        }
    }

    /// # Safety
    /// Live unique allocation and exact backend Layout from this origin.
    unsafe fn deallocate(&self, ordinary: &LockedHeap, base: NonNull<u8>, layout: Layout)
    {
        match self
        {
            Self::Ordinary(_) =>
            unsafe { ordinary.lock().deallocate(base, layout) },
            Self::Protected(receipt) =>
            unsafe { receipt.deallocate(base, layout) }
        }
    }
}

/// The static parent receipt counts actual selected-heap occupancy once.
/// An optional child receipt attributes that same backing to its owner; the
/// child counter is not additional physical capacity or a second allocation.
enum Attribution<R>
{
    System(Option<AllocationAdmission<NativeInvocationGate>>),
    Domain(R)
}
pub(crate) struct Reservation
{
    parent:      ParentReceipt,
    attribution: Attribution<Reserved<NativeInvocationGate>>
}
pub(crate) struct Receipt
{
    parent:      ParentReceipt,
    attribution: Attribution<Committed<NativeInvocationGate>>
}
impl Selection<'_>
{
    pub(crate) fn reserve(self, bytes: u64, recovery: Option<&recovery::Permit>)
                          -> Result<Reservation, Error>
    {
        let (category, attribution) = match self
        {
            Self::System(class) => (budget::Category::System(class), Attribution::System(None)),
            Self::Operation(OperationTarget::System { class,
                                                      admission }) =>
            {
                return admission.with_admission(|| {
                                    let parent =
                                        ParentReceipt::reserve(budget::Category::System(*class),
                                                               bytes,
                                                               recovery)?;
                                    Ok(Reservation { parent,
                                            attribution:
                                                Attribution::System(Some(admission.clone())) })
                                })
                                .map_err(Error::Operation)?;
            }
            Self::Operation(OperationTarget::Owner { account,
                                                     class }) =>
            {
                let reserved = match class
                               {
                                   MemoryClass::Kernel => account.reserve_kernel(bytes),
                                   MemoryClass::Application => account.reserve_application(bytes),
                                   _ => return Err(Error::Layout)
                               }.map_err(Error::Account)?;
                (budget::Category::Domain, Attribution::Domain(reserved))
            }
            Self::Owner { account,
                          class } =>
            {
                let reserved = match class
                               {
                                   MemoryClass::Kernel => account.reserve_kernel(bytes),
                                   MemoryClass::Application => account.reserve_application(bytes),
                                   _ => return Err(Error::Layout)
                               }.map_err(Error::Account)?;
                (budget::Category::Domain, Attribution::Domain(reserved))
            }
        };
        let parent = ParentReceipt::reserve(category, bytes, recovery)?;
        Ok(Reservation { parent,
                         attribution })
    }
}
impl Reservation
{
    fn commit(self) -> Receipt
    {
        let attribution = match self.attribution
        {
            Attribution::System(admission) => Attribution::System(admission),
            Attribution::Domain(reserved) => Attribution::Domain(reserved.commit())
        };
        Receipt { parent: self.parent,
                  attribution }
    }
}
impl Receipt
{
    pub(crate) fn reserve_replacement(&self, selected: Selection<'_>, bytes: u64)
                                      -> Result<Reservation, Error>
    {
        let attribution = match &self.attribution
        {
            Attribution::System(Some(admission)) =>
            {
                match admission.with_admission(|| self.parent.reserve_replacement(bytes))
                {
                    Ok(parent) =>
                    {
                        return Ok(Reservation { parent:      parent?,
                                                attribution:
                                                    Attribution::System(Some(admission.clone())) });
                    }
                    // System's ORIGINAL static authority remains accepting;
                    // current caller/class cannot retarget this old block.
                    Err(mac_object_table::scope::Failure::Closed) => Attribution::System(None),
                    Err(error) => return Err(Error::Operation(error))
                }
            }
            Attribution::System(None) => Attribution::System(None),
            Attribution::Domain(receipt) =>
            {
                let reservation =
                    match receipt.reserve_replacement(bytes)
                    {
                        Err(error @ mac_kernel_model::account::Error::Closed { .. }) =>
                        {
                            match selected
                            {
                                Selection::Operation(OperationTarget::Owner { account,
                                                                              .. }) =>
                                {
                                    receipt.reserve_replacement_in(bytes, account)
                                }
                                _ => Err(error)
                            }
                        }
                        result => result
                    }.map_err(Error::Account)?;
                Attribution::Domain(reservation)
            }
        };
        let parent = self.parent.reserve_replacement(bytes)?;
        Ok(Reservation { parent,
                         attribution })
    }
}

struct Header
{
    backing: Layout,
    receipt: Receipt
}
struct BlockLayout
{
    backing: Layout,
    payload: usize
}
impl BlockLayout
{
    fn new(request: Layout) -> Result<Self, Error>
    {
        let (combined, payload) =
            Layout::new::<Header>().extend(request).map_err(|_| Error::Layout)?;
        // Match the backend's exact rounding. Padding the whole extent to the
        // request's alignment would unnecessarily charge large aligned blocks.
        let backing = HoleList::align_layout(combined).map_err(|_| Error::Layout)?;
        Ok(Self { backing,
                  payload })
    }
}

pub(crate) struct ResidentHeap
{
    heap: LockedHeap
}
impl ResidentHeap
{
    pub(crate) const fn empty() -> Self
    {
        Self { heap: LockedHeap::empty() }
    }

    /// # Safety
    /// This uniquely owned permanent range is initialized exactly once, before
    /// the first allocation. It excludes the protected recovery partition.
    pub(crate) unsafe fn initialize(&self, start: *mut u8, length: usize)
    {
        with_masked(|| {
            // SAFETY: caller supplies exclusive reset-lifetime heap storage.
            unsafe {
                self.heap.lock().init(start, length);
            }
            budget::initialize(self.heap.lock().size() as u64);
        });
    }

    pub(crate) fn statistics(&self) -> (usize, usize)
    {
        with_masked(|| {
            let heap = self.heap.lock();
            (heap.used(), heap.size())
        })
    }

    pub(crate) fn available(&self) -> usize
    {
        with_masked(|| self.heap.lock().free())
    }

    #[cfg(feature = "emergency-qualification")]
    pub(crate) fn qualify_while_locked(&self, call: impl FnOnce() -> core::convert::Infallible)
                                       -> !
    {
        match with_masked(|| {
                  let _held = self.heap.lock();
                  call()
              }) {}
    }

    fn allocate(&self, block: BlockLayout, reservation: Reservation) -> *mut u8
    {
        let allocated = reservation.parent.allocate(&self.heap, block.backing);
        let Ok(base) = allocated
        else
        {
            // The heap guard has gone before reservation rollback can dispose
            // of its original account or its final foundation control block.
            drop(reservation);
            return ptr::null_mut();
        };
        let header = Header { backing: block.backing,
                              receipt: reservation.commit() };
        // SAFETY: the unique live block contains aligned header then payload;
        // nothing is published to the caller before the receipt is installed.
        unsafe {
            base.as_ptr().cast::<Header>().write(header);
            base.as_ptr().add(block.payload)
        }
    }
}

// SAFETY: only this boundary handles raw allocations. The actual backend and
// account selection are protected against local preemption; locks never escape.
unsafe impl GlobalAlloc for ResidentHeap
{
    unsafe fn alloc(&self, layout: Layout) -> *mut u8
    {
        with_masked(|| {
            let Ok(block) = BlockLayout::new(layout)
            else
            {
                return ptr::null_mut();
            };
            let Ok(reservation) = crate::context::reserve_allocation(block.backing.size() as u64)
            else
            {
                return ptr::null_mut();
            };
            self.allocate(block, reservation)
        })
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout)
    {
        with_masked(|| {
            // SAFETY: GlobalAlloc requires the original live pointer and its
            // current layout. Size changes retain alignment/prefix position.
            let payload = match BlockLayout::new(layout)
            {
                Ok(block) => block.payload,
                Err(_) =>
                {
                    crate::machine::emergency::enter(crate::emergency::Kind::SystemPanic,
                                                     format_args!("Resident deallocation \
                                                                   supplied an impossible \
                                                                   allocation layout"))
                }
            };
            unsafe {
                let base = pointer.sub(payload);
                let header = base.cast::<Header>().read();
                header.receipt
                      .parent
                      .deallocate(&self.heap, NonNull::new_unchecked(base), header.backing);
                // Backing is free and no heap guard is held before either the
                // static parent or original account receives its exact refund.
                drop(header);
            }
        });
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8
    {
        with_masked(|| {
            let Ok(old) = BlockLayout::new(layout)
            else
            {
                return ptr::null_mut();
            };
            let Ok(request) = Layout::from_size_align(size, layout.align())
            else
            {
                return ptr::null_mut();
            };
            // SAFETY: caller supplies a live allocation of the given layout.
            let header = unsafe { &*pointer.sub(old.payload).cast::<Header>() };
            if size <= header.backing.size() - old.payload
            {
                // Retain the existing capacity and its original charge. No
                // new admission is needed, including on a stopped owner.
                return pointer;
            }
            let Ok(block) = BlockLayout::new(request)
            else
            {
                return ptr::null_mut();
            };
            let Ok(reservation) =
                crate::context::reserve_replacement(&header.receipt, block.backing.size() as u64)
            else
            {
                return ptr::null_mut();
            };
            let replacement = self.allocate(block, reservation);
            if !replacement.is_null()
            {
                // Both allocations remain charged until copy completes. On
                // refusal the old allocation and receipt are untouched.
                unsafe {
                    ptr::copy_nonoverlapping(pointer, replacement, layout.size().min(size));
                    self.dealloc(pointer, layout);
                }
            }
            replacement
        })
    }
}
