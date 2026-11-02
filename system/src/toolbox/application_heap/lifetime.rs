//! Actual canonical Application heap cleanup. Stop is not memory quiescence:
//! every real process thread/claim and record mutation must have finished first.
#![forbid(unsafe_code)]
use mac_object_table::{domain::ReleasedResource,
                       registry::{CleanupFailure as _, Release, Source, Specification}};

use super::*;
use crate::toolbox::handles::{CapacityPolicy, KindCode, cleanup::Failure};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CleanupCause
{
    Unavailable,
    Threads,
    Readers
    {
        count: usize
    },
    Mutation,
    Fault(heap::Error),
    Remaining
    {
        allocations: usize,
        extents:     usize,
        preparing:   usize
    }
}
impl core::fmt::Display for CleanupCause
{
    fn fmt(&self, out: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match self
        {
            Self::Unavailable =>
            {
                out.write_str("Application heap manager is unavailable; exact resource ticket \
                               retained")
            }
            Self::Threads =>
            {
                out.write_str("Application heap awaits actual process thread/claim/context \
                               quiescence")
            }
            Self::Readers { count } =>
            {
                write!(out, "Application heap has {count} actual outstanding reader/mutation pins")
            }
            Self::Mutation =>
            {
                out.write_str("Application allocation still has an exclusive mutation or \
                               unpublished payload")
            }
            Self::Fault(cause) =>
            {
                write!(out, "Application heap cleanup retains original backing after {cause}")
            }
            Self::Remaining { allocations,
                              extents,
                              preparing } =>
            {
                write!(out,
                       "Application heap retains {allocations} allocations, {extents} extents and \
                        {preparing} metadata publications")
            }
        }
    }
}
#[derive(Clone, Copy)]
pub(crate) enum Kind
{
    Root,
    RootStorage,
    Allocation,
    AllocationStorage
}
impl Kind
{
    pub(crate) const ALL: [Self; 4] =
        [Self::Root, Self::RootStorage, Self::Allocation, Self::AllocationStorage];

    pub(crate) fn specification(self) -> Specification<'static>
    {
        let (kind, label, capacity) = match self
        {
            Self::Root =>
            {
                (*b"AHEP",
                 "Application heaps",
                 CapacityPolicy::HardBound { slots:  1,
                                             reason: "One canonical default Application heap per \
                                                      Process" })
            }
            Self::RootStorage =>
            {
                (*b"AHST",
                 "Application heap root backing",
                 CapacityPolicy::HardBound { slots:  1,
                                             reason: "One canonical heap-root value array per \
                                                      owner" })
            }
            Self::Allocation =>
            {
                (*b"AALC", "Application allocations", CapacityPolicy::GrowableUntilMemory)
            }
            Self::AllocationStorage =>
            {
                (*b"AAST",
                 "Application allocation record backing",
                 CapacityPolicy::HardBound { slots:  1,
                                             reason: "One allocation-record value array per owner" })
            }
        };
        Specification { kind: KindCode(kind),
                        label,
                        provider: "System Application heap",
                        source: Source::Resident,
                        capacity }
    }
}
impl Release<dyn crate::toolbox::handles::cleanup::context::Context, Failure> for Kind
{
    fn release(&self, _: &mut (dyn crate::toolbox::handles::cleanup::context::Context + 'static),
               ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        if ticket.key().kind() != self.specification().kind
        {
            return Err(Failure::registration_mismatch(ticket));
        }
        let Some(manager) = MANAGER.get()
        else
        {
            return Err(Failure::application_heap(ticket, CleanupCause::Unavailable));
        };
        match self
        {
            Self::RootStorage | Self::AllocationStorage =>
            {
                let outcome = interrupts::with_masked(|| {
                    let mut manager = manager.lock();
                    let pending = match self
                    {
                        Self::RootStorage =>
                        {
                            manager.roots.publication_preparations(ticket.key().owner())
                        }
                        _ => manager.allocations.publication_preparations(ticket.key().owner())
                    };
                    if pending != 0
                    {
                        return BackingRelease::Pending { ticket,
                                                         pending };
                    }
                    let result = match self
                    {
                        Self::RootStorage =>
                        {
                            manager.roots
                                   .release_storage_with_backing(ticket)
                                   .map(EitherBacking::Root)
                        }
                        _ =>
                        {
                            manager.allocations
                                   .release_storage_with_backing(ticket)
                                   .map(EitherBacking::Allocation)
                        }
                    };
                    BackingRelease::Result(result)
                });
                match outcome
                {
                    BackingRelease::Pending { ticket,
                                              pending } =>
                    {
                        Err(Failure::application_heap(ticket,
                                                      CleanupCause::Readers { count: pending }))
                    }
                    BackingRelease::Result(Ok(owner)) =>
                    {
                        drop(owner);
                        Ok(())
                    }
                    BackingRelease::Result(Err(cause)) => Err(Failure::from(cause))
                }
            }
            Self::Allocation =>
            {
                let record = interrupts::with_masked(|| {
                    manager.lock().allocations.releasing(&ticket).cloned()
                });
                let Some(record) = record
                else
                {
                    return Err(Failure::registration_mismatch(ticket));
                };
                if interrupts::with_masked(|| {
                    crate::machine::scheduler::has_threads(record.heap.owner.owner().incarnation())
                })
                {
                    return Err(Failure::application_heap(ticket, CleanupCause::Threads));
                }
                let readers = Arc::strong_count(&record);
                if readers != 2
                {
                    return Err(Failure::application_heap(ticket,CleanupCause::Readers{count:readers.saturating_sub(2)}));
                }
                if let Err(cause) = allocation::release_for_cleanup(&record, &ticket)
                {
                    return Err(Failure::application_heap(ticket, cause));
                }
                let result = interrupts::with_masked(|| {
                    manager.lock().allocations.release_with_backing(ticket)
                });
                // The local alias must end BEFORE Removed's actual payload ACK.
                drop(record);
                match result
                {
                    Ok(removed) =>
                    {
                        removed.dispose();
                        Ok(())
                    }
                    Err(cause) => Err(Failure::from(cause))
                }
            }
            Self::Root =>
            {
                let root =
                    interrupts::with_masked(|| manager.lock().roots.releasing(&ticket).cloned());
                let Some(root) = root
                else
                {
                    return Err(Failure::registration_mismatch(ticket));
                };
                root.closing.store(true, core::sync::atomic::Ordering::Release);
                if interrupts::with_masked(|| {
                    crate::machine::scheduler::has_threads(root.owner.owner().incarnation())
                })
                {
                    return Err(Failure::application_heap(ticket, CleanupCause::Threads));
                }
                let readers = Arc::strong_count(&root);
                if readers != 2
                {
                    return Err(Failure::application_heap(ticket,CleanupCause::Readers{count:readers.saturating_sub(2)}));
                }
                root.reap();
                let retired = interrupts::with_masked(|| {
                    let mut manager = manager.lock();
                    (manager.allocations.reap_publication_capacity(root.owner.owner()),
                     manager.roots.reap_publication_capacity(root.owner.owner()))
                });
                drop(retired);
                if let Some(cause) = root.cleanup_refusal()
                {
                    return Err(Failure::application_heap(ticket, cause));
                }
                let result =
                    interrupts::with_masked(|| manager.lock().roots.release_with_backing(ticket));
                drop(root);
                match result
                {
                    Ok(removed) =>
                    {
                        removed.dispose();
                        Ok(())
                    }
                    Err(cause) => Err(Failure::from(cause))
                }
            }
        }
    }
}

enum BackingRelease
{
    Pending
    {
        ticket:  ReleasedResource,
        pending: usize
    },
    Result(Result<EitherBacking, mac_object_table::domain::CompletionError>)
}

enum EitherBacking
{
    Root(Option<mac_object_table::store::RetiredOwner<Arc<Heap>, NativeInvocationGate>>),
    Allocation(Option<mac_object_table::store::RetiredOwner<Arc<allocation::Allocation>,
                                                             NativeInvocationGate>>)
}
impl Drop for EitherBacking
{
    fn drop(&mut self)
    {
        match self
        {
            Self::Root(owner) => drop(owner.take()),
            Self::Allocation(owner) => drop(owner.take())
        }
    }
}
