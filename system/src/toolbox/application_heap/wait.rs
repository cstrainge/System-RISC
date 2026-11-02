//! Actual heap-backing pins for native WAIT32. This proves mapped backing
//! lifetime, not suballocation/object lifetime or private page-table isolation.
#![forbid(unsafe_code)]
use core::{num::NonZeroU64,
           sync::atomic::{AtomicBool, AtomicU32, Ordering}};

use super::*;

static REAP_PENDING: AtomicBool = AtomicBool::new(false);

pub(super) struct Region
{
    incarnation: NonZeroU64,
    bytes:       u64,
    accepting:   AtomicBool
}
impl Region
{
    pub(super) fn prepare(bytes: u64) -> Result<Arc<Self>, Error>
    {
        let incarnation = mac_object_table::domain::issue_correlation_identity()
            .map_err(|_| Error::Pool(PoolError::IdentityExhausted))?;
        Arc::try_new(Self { incarnation,
                            bytes,
                            accepting: AtomicBool::new(true) }).map_err(|_| {
            Error::Metadata { stage:     "wait extent identity",
                              requested: 1 }
        })
    }

    pub(super) fn bytes(&self) -> u64
    {
        self.bytes
    }

    pub(super) fn accepting(&self) -> bool
    {
        self.accepting.load(Ordering::Acquire)
    }

    pub(super) fn close(&self)
    {
        self.accepting.store(false, Ordering::Release);
        crate::machine::scheduler::address_wait_changed();
    }
}

/// Matching metadata only. Construction requires a genuine retained root and
/// extent; copying this value cannot retain storage or grant memory access.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HeapWaitIdentity
{
    root:   CompletionKey,
    extent: NonZeroU64,
    offset: usize
}

/// Scheduler consumers detach this actual owner and call release outside their
/// guards. Drop is a non-reaping fallback: canonical root/deferred extent owners
/// remain authoritative while these extra reader references disappear.
pub(crate) struct HeapWaitPin
{
    extent:   Option<Arc<Extent>>,
    region:   Option<Arc<Region>>,
    heap:     Option<Arc<Heap>>,
    owner:    OwnerRef,
    identity: HeapWaitIdentity,
    address:  usize
}
impl HeapWaitPin
{
    pub(crate) fn identity(&self) -> HeapWaitIdentity
    {
        self.identity
    }

    pub(crate) fn address(&self) -> usize
    {
        self.address
    }

    /// Check while holding the scheduler's actual wait admission lock. The
    /// invalidation sweep is required as well: this flag alone is not atomic
    /// compare/enqueue/cancellation or an execution authority.
    pub(crate) fn accepting(&self) -> bool
    {
        self.heap.as_ref().is_some_and(|heap| heap.wait_accepting())
        && self.region.as_ref().is_some_and(|region| region.accepting())
    }

    pub(crate) fn release(mut self)
    {
        self.finish(true);
    }

    fn finish(&mut self, reap: bool)
    {
        // Drop the marker FIRST. The deferred retirement node retains its
        // marker until the actual extent unwrap succeeds, so the remaining
        // extent reader prevents a concurrent reaper from making this marker
        // the final charged metadata owner under a scheduler guard.
        drop(self.region.take());
        drop(self.extent.take());
        let Some(heap) = self.heap.take()
        else
        {
            return;
        };
        if reap
        {
            heap.reap();
        }
        // Signal only actual deferred retirement. Heap methods and Drop never
        // acquire SCHEDULER, enumerate roots or run a reaper under its guard.
        heap.notify_wait_release();
        drop(heap);
        if !self.owner.accepting()
        {
            crate::toolbox::processes::retirement::resources_released();
        }
    }
}
impl Drop for HeapWaitPin
{
    fn drop(&mut self)
    {
        self.finish(false);
    }
}

/// Authenticate against the current original Process root. None means only
/// that no live heap extent covers the complete atomic; authority, fault and
/// account failures retain their typed identity. No bytes are read here.
pub(crate) fn pin(executor: &crate::context::ApplicationExecutor<'_>, address: usize)
                  -> Result<Option<HeapWaitPin>, Error>
{
    if address == 0
       || address % core::mem::align_of::<AtomicU32>() != 0
       || address.checked_add(core::mem::size_of::<AtomicU32>()).is_none()
    {
        return Err(Error::Layout);
    }
    let heap = bind(executor)?;
    let Some((extent, region)) = Heap::wait_extent(&heap, address)?
    else
    {
        return Ok(None);
    };
    let identity = HeapWaitIdentity { root:   executor.heap().key(),
                                      extent: region.incarnation,
                                      offset: address - extent.base() };
    Ok(Some(HeapWaitPin { extent: Some(extent),
                          region: Some(region),
                          owner: heap.owner.clone(),
                          heap: Some(heap),
                          identity,
                          address }))
}

pub(super) fn request_reap()
{
    REAP_PENDING.store(true, Ordering::Release);
    crate::machine::scheduler::wake::request();
}

/// Guard-free canonical maintenance. Root enumeration occurs only after a real
/// release encounters pending physical retirement. No allocation or owning
/// destructor is performed under the manager guard; each root is visited at
/// most once in this turn, including when its reaper is temporarily busy.
pub(crate) fn pump()
{
    if !REAP_PENDING.swap(false, Ordering::AcqRel)
    {
        return;
    }
    let mut after = 0;
    loop
    {
        let next = with_manager(|manager| {
            manager.roots
                   .retained_values()
                   .filter(|heap| {
                       heap.owner.owner().incarnation() > after
                       && heap.wait_dirty.load(Ordering::Acquire)
                   })
                   .min_by_key(|heap| heap.owner.owner().incarnation())
                   .map(|heap| {
                       heap.wait_dirty.store(false, Ordering::Release);
                       heap.clone()
                   })
        });
        let Ok(Some(heap)) = next
        else
        {
            break;
        };
        after = heap.owner.owner().incarnation();
        let owner = heap.owner.clone();
        heap.reap();
        drop(heap);
        if !owner.accepting()
        {
            crate::toolbox::processes::retirement::resources_released();
        }
    }
}
