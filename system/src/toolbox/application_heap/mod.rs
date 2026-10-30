//! Canonical process Application heaps. Payload backing comes from actual buddy
//! extents; resident metadata retains the original process Kernel attribution.
#![deny(unsafe_code)]
use alloc::sync::Arc;
use core::fmt;

use mac_abi::{ACCESS_DENIED, BAD_ABI, BAD_ARGUMENT, NO_MEMORY, NOT_SUPPORTED, SERVICE_BUSY, Status};
use mac_kernel_model::{account, heap};
use mac_object_table::{domain::{DomainId, DomainKind, PoolError},
                       store::{CompletionKey, PreparedError, PublicationPermit, Store}};
use spinning_top::Spinlock;

use crate::{machine::{interrupts::{self, NativeInvocationGate},
                      pages,
                      scheduler::accounting::AllocationAuthority},
            toolbox::handles::OwnerRef};

mod allocation;
mod storage;
mod native;
pub(crate) mod wait;
pub(crate) mod lifetime;
use allocation::Allocation;
pub(crate) use lifetime::CleanupCause;
pub(crate) use native::request;
use storage::Heap;

type Extent = heap::Extent<'static, 'static, NativeInvocationGate>;
type Span = heap::AllocationSpan<'static, 'static, NativeInvocationGate>;
type Reservation = heap::SpanReservation<'static, 'static, NativeInvocationGate>;
type Empty = heap::EmptyExtent<'static, 'static, NativeInvocationGate>;

#[derive(Clone)]
pub(crate) struct RootBinding
{
    key:   CompletionKey,
    owner: DomainId
}
impl RootBinding
{
    pub(crate) fn key(&self) -> CompletionKey
    {
        self.key
    }

    pub(crate) fn owner(&self) -> DomainId
    {
        self.owner
    }
}

#[derive(Debug)]
pub(crate) enum Error
{
    Unavailable,
    AlreadyInitialized,
    Authority,
    Root,
    Layout,
    Pointer,
    Busy,
    Metadata
    {
        stage:     &'static str,
        requested: usize
    },
    Account(account::Error),
    Pool(PoolError),
    Publication(PreparedError),
    Extent(pages::ApplicationExtentError),
    Span(heap::Error),
    Rollback
    {
        original: Status,
        cause:    heap::Error
    },
    Kinds(crate::toolbox::handles::KindAccessError)
}
impl Error
{
    pub(crate) fn status(&self) -> Status
    {
        match self
        {
            Self::Unavailable=>NOT_SUPPORTED,
            Self::AlreadyInitialized|Self::Root=>BAD_ABI,
            Self::Authority=>ACCESS_DENIED,
            Self::Layout|Self::Pointer=>BAD_ARGUMENT,
            Self::Busy=>SERVICE_BUSY,
            Self::Metadata{..}=>NO_MEMORY,
            Self::Account(cause)=>crate::allocation::account_status(cause),
            Self::Pool(cause)=>crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(*cause)),
            Self::Publication(PreparedError::Pool(cause))=>Self::Pool(*cause).status(),
            Self::Publication(PreparedError::CapacityChanged)=>SERVICE_BUSY,
            Self::Publication(_)|Self::Span(_)|Self::Rollback{..}=>BAD_ABI,
            Self::Extent(cause)=>cause.status(),
            Self::Kinds(cause)=>cause.status()
        }
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Unavailable => out.write_str("Application heap service has not been initialized"),
            Self::AlreadyInitialized =>
            {
                out.write_str("Application heap service is already initialized")
            }
            Self::Authority =>
            {
                out.write_str("Application heap requires the genuine baseline Process \
                               executor/account")
            }
            Self::Root =>
            {
                out.write_str("Application heap binding does not match its canonical live root")
            }
            Self::Layout =>
            {
                out.write_str("Application allocation has an invalid size/alignment/layout")
            }
            Self::Pointer =>
            {
                out.write_str("Application allocation pointer/layout does not match its original \
                               live record")
            }
            Self::Busy =>
            {
                out.write_str("Application allocation is owned by an active mutation or cleanup \
                               ticket")
            }
            Self::Metadata { stage,
                             requested } =>
            {
                write!(out,
                       "Application heap {stage} metadata refused ({requested} entries requested)")
            }
            Self::Account(cause) => cause.fmt(out),
            Self::Pool(cause) => cause.fmt(out),
            Self::Publication(cause) => cause.fmt(out),
            Self::Extent(cause) => cause.fmt(out),
            Self::Span(cause) => cause.fmt(out),
            Self::Rollback { original,
                             cause } =>
            {
                write!(out,
                       "Application heap refusal {original} also retained unpublished backing \
                        after rollback failed: {cause}")
            }
            Self::Kinds(cause) =>
            {
                write!(out, "Application heap lifetime registration unavailable: {cause:?}")
            }
        }
    }
}
impl From<account::Error> for Error
{
    fn from(c: account::Error) -> Self
    {
        Self::Account(c)
    }
}
impl From<PoolError> for Error
{
    fn from(c: PoolError) -> Self
    {
        Self::Pool(c)
    }
}
impl From<heap::Error> for Error
{
    fn from(c: heap::Error) -> Self
    {
        Self::Span(c)
    }
}

struct Manager
{
    roots:       Store<Arc<Heap>, NativeInvocationGate>,
    allocations: Store<Arc<Allocation>, NativeInvocationGate>
}
static MANAGER: spin::Once<Spinlock<Manager>> = spin::Once::new();

/// Called once by the genuine boot owner after handle registration, before any
/// Process becomes runnable. Request paths never lazily allocate this singleton.
pub(crate) fn initialize() -> Result<(), Error>
{
    if !crate::context::current_thread_context().is_bootstrap()
    {
        return Err(Error::Authority);
    }
    if MANAGER.get().is_some()
    {
        return Err(Error::AlreadyInitialized);
    }
    let kinds=crate::toolbox::handles::registry(|registry|{
        Ok::<_,mac_object_table::registry::Error>([registry.get(crate::toolbox::handles::KindCode(*b"AHEP"))?,
            registry.get(crate::toolbox::handles::KindCode(*b"AHST"))?,
            registry.get(crate::toolbox::handles::KindCode(*b"AALC"))?,
            registry.get(crate::toolbox::handles::KindCode(*b"AAST"))?])
    }).map_err(Error::Kinds)?.map_err(|cause|Error::Kinds(crate::toolbox::handles::KindAccessError::Registry(cause)))?;
    let [root, root_storage, allocation, allocation_storage] = kinds;
    let manager = crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
        Ok::<_, Error>(Manager { roots:       Store::owned_prepared(root, root_storage)?,
                                 allocations: Store::owned_prepared(allocation,
                                                                    allocation_storage)? })
    })?;
    // Startup is single admitted boot-owner execution. No guard spans the
    // fallible preparation above; no worker can enter this callsite.
    MANAGER.call_once(|| Spinlock::new(manager));
    Ok(())
}

fn with_manager<R>(operation: impl FnOnce(&mut Manager) -> R) -> Result<R, Error>
{
    let manager = MANAGER.get().ok_or(Error::Unavailable)?;
    Ok(interrupts::with_masked(|| operation(&mut manager.lock())))
}

/// Genuine empty root plus its actual unpublished facility permit. No payload
/// extent exists yet; field order releases Kernel heap metadata before permit.
pub(crate) struct PreparedRoot
{
    heap:   Arc<Heap>,
    permit: PublicationPermit<NativeInvocationGate>
}
impl PreparedRoot
{
    pub(crate) fn dispose(self)
    {
        drop(self)
    }
}
pub(crate) struct RootPublicationFailure
{
    pub(crate) cause:    Error,
    pub(crate) prepared: PreparedRoot
}
struct PreparationProgress(OwnerRef);
impl Drop for PreparationProgress
{
    fn drop(&mut self)
    {
        // The temporary allocation selection has already been restored/dropped
        // before this outer marker, including errors before AHEP publication.
        if !self.0.accepting()
        {
            crate::toolbox::processes::retirement::resources_released();
        }
    }
}

fn root_permit(owner: &OwnerRef) -> Result<PublicationPermit<NativeInvocationGate>, Error>
{
    loop
    {
        let plan = with_manager(|m| m.roots.plan_publication(owner))??;
        let prepared = plan.prepare()?;
        match with_manager(|m| m.roots.admit_prepared(prepared))?
        {
            Ok((permit, retired)) =>
            {
                retired.dispose();
                return Ok(permit);
            }
            Err(failure) =>
            {
                let cause = failure.cause;
                drop(failure.preparation);
                if cause != PreparedError::CapacityChanged
                {
                    return Err(Error::Publication(cause));
                }
            }
        }
    }
}

pub(crate) fn prepare_root(owner: OwnerRef, allocation: AllocationAuthority)
                           -> Result<PreparedRoot, Error>
{
    if owner.owner().kind() != DomainKind::Process || allocation.owner() != owner.owner()
    {
        return Err(Error::Authority);
    }
    let _progress = PreparationProgress(owner.clone());
    crate::allocator::with_target(crate::allocator::Target::Owner{account:allocation.clone(),
        class:mac_kernel_model::policy::MemoryClass::Kernel},||{
        let permit=root_permit(&owner)?;
        let heap=Arc::try_new(Heap::new(owner,allocation)).map_err(|_|Error::Metadata{stage:"root",requested:1})?;
        Ok(PreparedRoot{heap,permit})
    })
}

pub(crate) fn publish_root(prepared: PreparedRoot) -> Result<RootBinding, RootPublicationFailure>
{
    let owner = prepared.heap.owner.owner();
    let PreparedRoot { heap,
                       permit } = prepared;
    let manager = match MANAGER.get()
    {
        Some(m) => m,
        None =>
        {
            return Err(RootPublicationFailure { cause:    Error::Unavailable,
                                                prepared: PreparedRoot { heap,
                                                                         permit } });
        }
    };
    let result = interrupts::with_masked(|| manager.lock().roots.publish_prepared(permit, heap));
    match result
    {
        Ok((publication, retired)) =>
        {
            retired.dispose();
            let (_, key) = publication.into_parts();
            Ok(RootBinding { key,
                             owner })
        }
        Err(failure) =>
        {
            Err(RootPublicationFailure { cause:    Error::Publication(failure.cause),
                                         prepared: PreparedRoot { heap:   failure.value,
                                                                  permit: failure.permit } })
        }
    }
}

fn bind(executor: &crate::context::ApplicationExecutor<'_>) -> Result<Arc<Heap>, Error>
{
    let binding = executor.heap();
    if binding.owner() != executor.owner().owner()
       || executor.allocation().owner() != binding.owner()
    {
        return Err(Error::Authority);
    }
    let heap = with_manager(|m| m.roots.completing(&binding.key()).cloned())?.ok_or(Error::Root)?;
    if heap.owner.owner() != binding.owner()
    {
        return Err(Error::Root);
    }
    Ok(heap)
}
