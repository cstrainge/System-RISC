//! The single audited application allocation pointer boundary. Numeric values
//! become pointers only while actual owned span/extent pins remain retained.
#![allow(unsafe_code)]
use core::alloc::Layout;

use mac_abi::application_heap as wire;

use super::*;

struct Request
{
    heap:  Option<Arc<Heap>>,
    owner: OwnerRef
}
impl Drop for Request
{
    fn drop(&mut self)
    {
        if let Some(heap) = self.heap.take()
        {
            heap.reap();
            drop(heap)
        }
        // No Account/heap/backing pin remains in this marker. Existing stopped
        // owner metadata drains through its actual canonical owner walk.
        if !self.owner.accepting()
        {
            crate::toolbox::processes::retirement::resources_released();
        }
    }
}

/// # Safety
/// The Call is a live/exclusive resident snapshot for this synchronous trap.
/// Public pointer numerics are translated through the actual Process extent
/// and checked against its canonical record/exact Layout before byte effects.
/// Raw allocation pointers do not acquire stale/ABA protection from AALC IDs.
pub(crate) unsafe fn request(executor: &crate::context::ApplicationExecutor<'_>,
                             call: &mut wire::Call)
                             -> Result<(), Error>
{
    // Public app growth cannot inherit a protected Finder placement permit.
    // The original executor/account/Subject stay authoritative; the scope ends
    // before the caller routes a typed failure to protected crash reporting.
    crate::context::without_recovery(|| {
        call.result = core::ptr::null_mut();
        call.detail_length = 0;
        if call.reserved != 0 || call.reserved2 != 0
        {
            return Err(Error::Layout);
        }
        match call.operation
        {
            wire::ALLOCATE | wire::ALLOCATE_ZEROED
                if call.pointer.is_null() && call.old_size == 0 && call.new_size != 0 =>
            {
                ()
            }
            wire::DEALLOCATE
                if !call.pointer.is_null() && call.old_size != 0 && call.new_size == 0 =>
            {
                ()
            }
            wire::REALLOCATE
                if !call.pointer.is_null() && call.old_size != 0 && call.new_size != 0 =>
            {
                ()
            }
            _ => return Err(Error::Layout)
        }
        // Layout/overflow refusal precedes address lookup, metadata or byte effects.
        let size = if call.operation == wire::DEALLOCATE || call.operation == wire::REALLOCATE
        {
            call.old_size
        }
        else
        {
            call.new_size
        };
        let layout = Layout::from_size_align(size, call.alignment).map_err(|_| Error::Layout)?;
        if !call.pointer.is_null()
           && (call.pointer.addr() % layout.align() != 0
               || call.pointer.addr().checked_add(layout.size()).is_none())
        {
            return Err(Error::Pointer);
        }
        let replacement = if call.operation == wire::REALLOCATE
        {
            Some(Layout::from_size_align(call.new_size, call.alignment).map_err(|_| Error::Layout)?)
        }
        else
        {
            None
        };
        let heap = bind(executor)?;
        let owner = heap.owner.clone();
        let request = Request { heap: Some(heap),
                                owner };
        let heap = request.heap.as_ref().ok_or(Error::Root)?;
        let supplied = if call.pointer.is_null()
        {
            0
        }
        else
        {
            heap.cpu_address(call.pointer.addr(), layout.size())?
        };
        match call.operation
        {
            wire::ALLOCATE | wire::ALLOCATE_ZEROED =>
            {
                let permit = allocation::allocation_permit(&heap)?;
                let credit = storage::Heap::index_credit(&heap)?;
                let draft = allocation::Draft::prepare(&heap, layout)?;
                if call.operation == wire::ALLOCATE_ZEROED
                {
                    // SAFETY: the actual factory-derived address is within this
                    // exclusive unpublished span, which stays pinned in Draft.
                    unsafe {
                        crate::machine::paging::user::zero_alias(draft.pointer, layout.size())
                    };
                }
                let user = heap.user_address(draft.pointer, layout.size())?;
                let _pointer = draft.publish(permit, credit)?;
                call.result = user as *mut u8;
            }
            wire::DEALLOCATE =>
            {
                allocation::Lease::take(&heap, supplied, layout)?.dispose()?;
            }
            wire::REALLOCATE =>
            {
                heap.admission()?;
                let old = allocation::Lease::take(&heap, supplied, layout)?;
                let layout = replacement.ok_or(Error::Layout)?;
                let replacement = allocation::Replacement::prepare(old, layout)?;
                let (old, new, bytes) = replacement.copy_bounds()?;
                // SAFETY: canonical record/exact layout were checked before access.
                // The exclusive old lease and distinct new live span remain pinned;
                // core partition ownership excludes overlap. Both addresses are
                // stable supervisor aliases of actual Process-owned extents;
                // public addresses are translated independently to user VAs.
                // No metadata/ledger/account/pool guard spans copying.
                unsafe { crate::machine::paging::user::copy_aliases(old, new, bytes) };
                let user = heap.user_address(new, layout.size())?;
                let _pointer = replacement.commit()?;
                call.result = user as *mut u8;
            }
            _ => return Err(Error::Layout)
        }
        Ok(())
    })
}

/// Factory-derived rounded extent metadata and its genuine physical owner.
struct ExtentBacking
{
    owner:    mac_object_table::domain::DomainId,
    progress: super::wait::Progress,
    extent:   Arc<Extent>,
    physical: mac_kernel_model::pages::PhysicalAddress,
    bytes:    usize
}
// SAFETY: only map_extent constructs this adapter, immediately from the checked
// native factory and its exact original reservation. Arc pins actual PageBlock
// and charge; no logical allocation pointer supplies physical coordinates.
unsafe impl crate::machine::paging::user::Backing for ExtentBacking
{
    fn heap_progress(&self) -> Option<super::wait::Progress>
    {
        Some(self.progress.clone())
    }

    fn owner(&self) -> mac_object_table::domain::DomainId
    {
        self.owner
    }

    fn cpu_base(&self) -> usize
    {
        self.extent.base()
    }

    fn physical(&self) -> mac_kernel_model::pages::PhysicalAddress
    {
        self.physical
    }

    fn bytes(&self) -> usize
    {
        self.bytes
    }
}
pub(super) fn map_extent(heap: &Arc<Heap>, extent: Arc<Extent>, bytes: u64)
                         -> Result<crate::machine::paging::user::Mapping, Error>
{
    let space = crate::machine::scheduler::current_user_space().map_err(Error::User)?;
    if space.owner() != heap.owner.owner()
    {
        return Err(Error::Authority);
    }
    let physical=pages::application_extent_physical(&extent,bytes)
        .map_err(|cause|Error::Extent(pages::ApplicationExtentError::Pages(cause)))?;
    let bytes = usize::try_from(bytes).map_err(|_| Error::Layout)?;
    let backing = heap.kernel(|| {
                          Arc::try_new(ExtentBacking { owner: heap.owner.owner(),
                                                       progress:
                                                           super::wait::Progress::new(heap),
                                                       extent,
                                                       physical,
                                                       bytes })
                      })
                      .map_err(|_| {
                          Error::Metadata { stage:     "user extent owner",
                                            requested: 1 }
                      })?;
    crate::machine::paging::user::Mapping::prepare(&space,backing,
        crate::machine::paging::user::Kind::Heap,
        crate::machine::paging::user::Access::ReadWrite).map_err(Error::User)
}
