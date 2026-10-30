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
/// Native GlobalAlloc's exclusive-live-pointer/exact-Layout contract applies to
/// free/realloc. The Call is live/exclusive for this synchronous trap. These raw
/// pointers do not acquire stale-handle/ABA protection from canonical AALC IDs.
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
                    unsafe { core::ptr::write_bytes(draft.pointer as *mut u8, 0, layout.size()) };
                }
                let pointer = draft.publish(permit, credit)?;
                call.result = pointer as *mut u8;
            }
            wire::DEALLOCATE =>
            {
                allocation::Lease::take(&heap, call.pointer.addr(), layout)?.dispose()?;
            }
            wire::REALLOCATE =>
            {
                heap.admission()?;
                let old = allocation::Lease::take(&heap, call.pointer.addr(), layout)?;
                let layout = replacement.ok_or(Error::Layout)?;
                let replacement = allocation::Replacement::prepare(old, layout)?;
                let (old, new, bytes) = replacement.copy_bounds()?;
                // SAFETY: canonical record/exact layout were checked before access.
                // The exclusive old lease and distinct new live span remain pinned;
                // core partition ownership excludes overlap. No metadata/ledger/
                // account/pool guard spans copying. Shared supervisor roots still
                // map the actual checked DirectMap; this claims no MMU isolation.
                unsafe { core::ptr::copy_nonoverlapping(old as *const u8, new as *mut u8, bytes) };
                call.result = replacement.commit()? as *mut u8;
            }
            _ => return Err(Error::Layout)
        }
        Ok(())
    })
}
