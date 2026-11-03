//! Actual heap-backing pins for native WAIT32. This proves mapped backing
//! lifetime, not suballocation/object lifetime or private page-table isolation.
#![forbid(unsafe_code)]
use core::{num::NonZeroU64,
           sync::atomic::{AtomicBool, AtomicU32, Ordering}};

use super::*;

static REAP_PENDING: AtomicBool = AtomicBool::new(false);
static MONITOR_PENDING: AtomicBool = AtomicBool::new(false);

/// Non-owning exact Heap wake capability. It cannot select an account, retain
/// physical pages or keep Heap -> Mapping -> Heap ownership cyclic.
#[derive(Clone)]
pub(crate) struct Progress(alloc::sync::Weak<Heap>);
impl Progress
{
    pub(super) fn new(heap: &Arc<Heap>) -> Self
    {
        Self(Arc::downgrade(heap))
    }

    pub(crate) fn notify(self)
    {
        let heap = self.0.upgrade();
        drop(self);
        if let Some(heap) = heap
        {
            heap.notify_wait_release();
            drop(heap);
        }
    }
}

pub(super) struct Region
{
    incarnation: NonZeroU64,
    bytes:       u64,
    accepting:   AtomicBool,
    ready:       AtomicBool,
    mapping:     spinning_top::Spinlock<MappingState>
}
struct MappingState
{
    mapping: Option<crate::machine::paging::user::Mapping>,
    busy:    bool
}
impl Region
{
    pub(super) fn prepare(bytes: u64, mapping: crate::machine::paging::user::Mapping)
                          -> Result<Arc<Self>, Error>
    {
        let incarnation = mac_object_table::domain::issue_correlation_identity()
            .map_err(|_| Error::Pool(PoolError::IdentityExhausted))?;
        Arc::try_new(Self { incarnation,
                            bytes,
                            accepting: AtomicBool::new(true),
                            ready:AtomicBool::new(false),
                            mapping:spinning_top::Spinlock::new(MappingState{mapping:Some(mapping),busy:false}) }).map_err(|_| {
            Error::Metadata { stage:     "wait extent identity",
                              requested: 1 }
        })
    }

    fn mapping_turn<T>(&self,
                       operation: impl FnOnce(&mut crate::machine::paging::user::Mapping)
                              -> Result<T, crate::machine::paging::user::Error>)
                       -> Result<T, Error>
    {
        let mut mapping = interrupts::with_masked(|| {
            let mut state = self.mapping.lock();
            if state.busy
            {
                return Err(Error::Busy);
            }
            let mapping = state.mapping.take().ok_or(Error::Root)?;
            state.busy = true;
            Ok(mapping)
        })?;
        let result = operation(&mut mapping);
        interrupts::with_masked(|| {
            let mut state = self.mapping.lock();
            state.mapping = Some(mapping);
            state.busy = false;
        });
        result.map_err(Error::UserMapping)
    }

    pub(super) fn publish(&self) -> Result<(), Error>
    {
        self.mapping_turn(|mapping| mapping.publish())?;
        self.ready.store(true, Ordering::Release);
        Ok(())
    }

    pub(super) fn quarantine_cause(&self) -> Option<crate::machine::paging::modules::CleanupCause>
    {
        interrupts::with_masked(|| {
            self.mapping.lock().mapping.as_ref().and_then(|mapping| mapping.quarantine_cause())
        })
    }

    pub(super) fn retire_mapping(&self) -> Result<(), Error>
    {
        self.mapping_turn(|m| m.retire())
    }

    pub(super) fn user_address(&self, cpu: usize, bytes: usize) -> Result<usize, Error>
    {
        if !self.accepting()
        {
            return Err(Error::Pointer);
        }
        interrupts::with_masked(|| {
            let state = self.mapping.lock();
            let mapping = state.mapping.as_ref().ok_or(Error::Busy)?;
            let offset = cpu.checked_sub(mapping.cpu_base()).ok_or(Error::Pointer)?;
            if offset.checked_add(bytes).is_none_or(|end| end > self.bytes as usize)
            {
                return Err(Error::Pointer);
            }
            mapping.user_base().checked_add(offset).ok_or(Error::Layout)
        })
    }

    pub(super) fn cpu_address(&self, address: usize, bytes: usize) -> Result<Option<usize>, Error>
    {
        if !self.accepting()
        {
            return Ok(None);
        }
        interrupts::with_masked(|| {
            let state = self.mapping.lock();
            let mapping = state.mapping.as_ref().ok_or(Error::Busy)?;
            let Some(offset) = address.checked_sub(mapping.user_base())
            else
            {
                return Ok(None);
            };
            if offset.checked_add(bytes).is_none_or(|end| end > self.bytes as usize)
            {
                return Ok(None);
            }
            mapping.cpu_base().checked_add(offset).map(Some).ok_or(Error::Layout)
        })
    }

    pub(super) fn bytes(&self) -> u64
    {
        self.bytes
    }

    pub(super) fn accepting(&self) -> bool
    {
        self.accepting.load(Ordering::Acquire) && self.ready.load(Ordering::Acquire)
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
    let Some(cpu) = heap.try_cpu_address(address, core::mem::size_of::<AtomicU32>())?
    else
    {
        return Ok(None);
    };
    let Some((extent, region)) = Heap::wait_extent(&heap, cpu)?
    else
    {
        return Ok(None);
    };
    let identity = HeapWaitIdentity { root:   executor.heap().key(),
                                      extent: region.incarnation,
                                      offset: cpu - extent.base() };
    Ok(Some(HeapWaitPin { extent: Some(extent),
                          region: Some(region),
                          owner: heap.owner.clone(),
                          heap: Some(heap),
                          identity,
                          address: cpu }))
}

pub(crate) fn monitor_released()
{
    MONITOR_PENDING.store(true, Ordering::Release);
    crate::machine::scheduler::wake::request();
}
pub(crate) fn request_reap()
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
    let readers = REAP_PENDING.swap(false, Ordering::AcqRel);
    let monitor = MONITOR_PENDING.swap(false, Ordering::AcqRel);
    if !readers && !monitor
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
                       && (heap.wait_dirty.load(Ordering::Acquire)
                           || (monitor && heap.has_retirement()))
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
