//! Process TLS belongs to the canonical ledger; slots belong to actual THRD
//! contexts. Native CURRENT never substitutes ProcessInfo's initial thread.
#![forbid(unsafe_code)]
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{fmt,
           sync::atomic::{AtomicBool, Ordering}};

use mac_abi::*;
use mac_object_table::{domain::{DomainId, DomainKind, PoolError},
                       store::{CompletionKey, PreparedError, PublicationPermit}};
use spinning_top::Spinlock;

use crate::{machine::{interrupts, scheduler::accounting::AllocationAuthority},
            toolbox::handles::{InvocationGate, OwnerRef}};
mod slots;
mod keys;
pub(crate) mod lifetime;
pub(crate) mod native;
pub(crate) use lifetime::CleanupCause;
pub(crate) use slots::{ThreadSlots, prepare_slots};
type Store<T> = mac_object_table::store::Store<T, InvocationGate>;
type Removal<T> = mac_object_table::store::Removal<T, InvocationGate>;
#[derive(Clone)]
pub(crate) struct RootBinding
{
    key:   CompletionKey,
    owner: DomainId
}
impl RootBinding
{
    pub(crate) fn owner(&self) -> DomainId
    {
        self.owner
    }
}
/// A sealed pin to this registered Process runtime admission root. System
/// failure closes new author/key admission while Finder's owner ledger remains
/// accepting for its actual Activity recipient handoff. Existing TLS/report
/// access is not revoked. Drop this temporary pin before the owner walk.
pub(crate) struct RuntimeAdmission
{
    root: Arc<Root>
}
impl RuntimeAdmission
{
    pub(crate) fn owner(&self) -> &OwnerRef
    {
        &self.root.owner
    }

    pub(crate) fn accepting(&self) -> bool
    {
        self.root.owner.accepting() && !self.root.closing.load(Ordering::Acquire)
    }

    pub(crate) fn close(&self)
    {
        self.root.closing.store(true, Ordering::Release);
    }
}
pub(crate) fn runtime_admission(binding: &RootBinding, owner: &OwnerRef)
                                -> Result<RuntimeAdmission, Error>
{
    Ok(RuntimeAdmission { root: root(binding, owner)? })
}
pub(super) struct Root
{
    owner:      OwnerRef,
    allocation: AllocationAuthority,
    positions:  Spinlock<Vec<bool>>,
    closing:    AtomicBool
}
impl Root
{
    fn kernel<R>(&self, action: impl FnOnce() -> R) -> R
    {
        crate::allocator::with_target(crate::allocator::Target::Owner {
            account: self.allocation.clone(), class: mac_kernel_model::policy::MemoryClass::Kernel
        }, action)
    }
}
pub(super) struct Position
{
    root: Arc<Root>,
    at:   usize
}
impl Drop for Position
{
    fn drop(&mut self)
    {
        interrupts::with_masked(|| {
            if let Some(position) = self.root.positions.lock().get_mut(self.at)
            {
                *position = false;
            }
        });
    }
}
pub(super) struct Key
{
    destructor: usize,
    _code:      Option<crate::images::ApplicationExecution>,
    position:   Position,
    anchor:     Spinlock<Option<Box<Deleted>>>,
    signal:     Arc<KeySignal>
}
struct KeySignal
{
    deleted: AtomicBool,
    owner:   OwnerRef
}
/// Ordinary key reads release the actual Arc before signaling reader progress.
/// The release signal owns no code, Root or AccountUse authority. No callback or
/// detached-list scan runs under a slots/context guard.
pub(super) struct KeyRead
{
    key:       Arc<Key>,
    _released: KeyReleased
}
struct KeyReleased
{
    signal: Arc<KeySignal>,
    notify: KeyNotify
}
struct KeyNotify
{
    deleted: bool,
    owner:   OwnerRef
}
impl KeyRead
{
    fn new(key: Arc<Key>) -> Self
    {
        let released = KeyReleased { signal: key.signal.clone(),
                                     notify: KeyNotify { deleted: false,
                                                         owner:   key.signal.owner.clone() } };
        Self { key,
               _released: released }
    }
}
impl core::ops::Deref for KeyRead
{
    type Target = Key;

    fn deref(&self) -> &Key
    {
        &self.key
    }
}
impl Drop for KeyReleased
{
    fn drop(&mut self)
    {
        // Fields then drop signal BEFORE notify. This cannot upgrade the Key
        // or become its last owning code/root reference under a context guard.
        // Charged signal bytes also leave before the final progress notice.
        self.notify.deleted = self.signal.deleted.load(Ordering::Acquire);
    }
}
impl Drop for KeyNotify
{
    fn drop(&mut self)
    {
        // Signal each actual reader release after deletion/stop. Requiring a
        // transient strong_count==1 loses concurrent final-reader wakeups.
        // Healthy live-key reads schedule no detached-list work.
        if self.deleted
        {
            request_reap();
        }
        if !self.owner.accepting()
        {
            crate::toolbox::processes::retirement::resources_released();
        }
    }
}
pub(super) struct Deleted
{
    removal: Option<Removal<Arc<Key>>>,
    next:    Option<Box<Deleted>>
}
struct Manager
{
    roots:   Store<Arc<Root>>,
    keys:    Store<Arc<Key>>,
    deleted: Option<Box<Deleted>>
}
static MANAGER: spin::Once<Spinlock<Manager>> = spin::Once::new();
static REAP_PENDING: AtomicBool = AtomicBool::new(false);
static REAP_RUNNING: AtomicBool = AtomicBool::new(false);
struct ReapTurn;
impl Drop for ReapTurn
{
    fn drop(&mut self)
    {
        REAP_RUNNING.store(false, Ordering::Release);
    }
}
#[derive(Debug)]
pub(crate) enum Error
{
    Authority,
    Context(crate::context::ApplicationExecutorError),
    Runtime(crate::machine::scheduler::application::RuntimeAdmissionError),
    Unavailable,
    Invalid,
    Missing,
    Busy,
    Ticket,
    Finished,
    Metadata(&'static str),
    Capacity
    {
        limit: usize
    },
    Rounds
    {
        remaining: usize
    },
    Pool(PoolError),
    Publication(PreparedError),
    Code(Status),
    Identity
}
impl Error
{
    pub(crate) fn status(&self) -> Status
    {
        match self {
            Self::Authority => ACCESS_DENIED,
            Self::Context(cause) => cause.status(),
            Self::Runtime(cause)=>cause.status(), Self::Missing => NOT_FOUND,
            Self::Busy => SERVICE_BUSY, Self::Finished => OPERATION_CANCELLED,
            Self::Metadata(_) | Self::Capacity { .. } => NO_MEMORY,
            Self::Pool(cause) => crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(*cause)),
            Self::Code(status) => *status,
            Self::Publication(PreparedError::Pool(cause)) => crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(*cause)),
            Self::Unavailable | Self::Publication(_) | Self::Identity => BAD_ABI,
            Self::Invalid | Self::Ticket | Self::Rounds { .. } => BAD_ARGUMENT
        }
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Authority =>
            {
                out.write_str("TLS requires the actual Process executor, original account and TLS \
                               root")
            }
            Self::Context(cause) => cause.fmt(out),
            Self::Runtime(cause) => cause.fmt(out),
            Self::Unavailable =>
            {
                out.write_str("native TLS manager has not been initialized by System")
            }
            Self::Invalid =>
            {
                out.write_str("invalid native TLS operation, reserved field or input combination")
            }
            Self::Missing =>
            {
                out.write_str("TLS key is stale, deleted or absent in the actual Process")
            }
            Self::Busy =>
            {
                out.write_str("TLS publication or current-thread destructor receipt is still in \
                               flight")
            }
            Self::Ticket =>
            {
                out.write_str("TLS destructor ACK does not match this actual thread's outstanding \
                               receipt")
            }
            Self::Finished => out.write_str("this actual thread has already finished TLS teardown"),
            Self::Metadata(stage) =>
            {
                write!(out, "original Process Kernel budget refused TLS {stage}")
            }
            Self::Capacity { limit } =>
            {
                write!(out, "Process TLS key capacity {limit} is exhausted; no slot was published")
            }
            Self::Rounds { remaining } =>
            {
                write!(out,
                       "TLS teardown exhausted eight rounds; {remaining} nonzero slots remain and \
                        their callbacks are not claimed complete")
            }
            Self::Pool(cause) => cause.fmt(out),
            Self::Publication(cause) => cause.fmt(out),
            Self::Code(status) =>
            {
                write!(out,
                       "TLS destructor executable-image admission refused with status {status}")
            }
            Self::Identity => out.write_str("shared native receipt identity facility is exhausted")
        }
    }
}
impl From<PoolError> for Error
{
    fn from(cause: PoolError) -> Self
    {
        Self::Pool(cause)
    }
}
fn manager<R>(operation: impl FnOnce(&mut Manager) -> R) -> Result<R, Error>
{
    let manager = MANAGER.get().ok_or(Error::Unavailable)?;
    Ok(interrupts::with_masked(|| operation(&mut manager.lock())))
}
pub(crate) fn initialize() -> Result<(), Error>
{
    if !crate::context::current_thread_context().is_bootstrap() || MANAGER.get().is_some()
    {
        return Err(Error::Authority);
    }
    let kinds = crate::toolbox::handles::registry(|registry| {
                    Ok::<_,mac_object_table::registry::Error>([
            registry.get(lifetime::Kind::Root.specification().kind)?,
            registry.get(lifetime::Kind::RootStorage.specification().kind)?,
            registry.get(lifetime::Kind::Key.specification().kind)?,
            registry.get(lifetime::Kind::KeyStorage.specification().kind)?])
                }).map_err(|_| Error::Unavailable)?
                  .map_err(|_| Error::Unavailable)?;
    let [root, root_storage, key, key_storage] = kinds;
    let prepared = crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
        Ok::<_, Error>(Manager { roots:   Store::owned_prepared(root, root_storage)?,
                                 keys:    Store::owned_prepared(key, key_storage)?,
                                 deleted: None })
    })?;
    MANAGER.call_once(|| Spinlock::new(prepared));
    Ok(())
}
fn permit<T>(owner: &OwnerRef, select: impl Fn(&mut Manager) -> &mut Store<T>)
             -> Result<PublicationPermit<InvocationGate>, Error>
{
    loop
    {
        let plan = manager(|manager| select(manager).plan_publication(owner))??;
        let prepared = plan.prepare()?;
        match manager(|manager| select(manager).admit_prepared(prepared))?
        {
            Ok((permit, old)) =>
            {
                old.dispose();
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
pub(crate) struct PreparedRoot
{
    root:   Arc<Root>,
    permit: PublicationPermit<InvocationGate>
}
impl PreparedRoot
{
    pub(crate) fn dispose(self)
    {
        drop(self);
    }
}
pub(crate) struct RootFailure
{
    pub(crate) cause:    Error,
    pub(crate) prepared: PreparedRoot
}
pub(crate) fn prepare_root(owner: OwnerRef, allocation: AllocationAuthority)
                           -> Result<PreparedRoot, Error>
{
    if owner.owner().kind() != DomainKind::Process || allocation.owner() != owner.owner()
    {
        return Err(Error::Authority);
    }
    let target = crate::allocator::Target::Owner { account: allocation.clone(),
                                                   class:
                                                       mac_kernel_model::policy::MemoryClass::Kernel };
    crate::allocator::with_target(target, || {
        let permit = permit(&owner, |manager| &mut manager.roots)?;
        let capacity = crate::machine::memory::TLS_KEY_CAPACITY as usize;
        let mut positions = Vec::new();
        positions.try_reserve_exact(capacity)
                 .map_err(|_| Error::Metadata("root position array"))?;
        positions.resize(capacity, false);
        let root = Arc::try_new(Root { owner,
                                       allocation,
                                       positions: Spinlock::new(positions),
                                       closing: AtomicBool::new(false) }).map_err(|_| {
                                                                             Error::Metadata("root")
                                                                         })?;
        Ok(PreparedRoot { root,
                          permit })
    })
}
pub(crate) fn publish_root(prepared: PreparedRoot) -> Result<RootBinding, RootFailure>
{
    let PreparedRoot { root,
                       permit } = prepared;
    let owner = root.owner.owner();
    let Some(manager) = MANAGER.get()
    else
    {
        return Err(RootFailure { cause:    Error::Unavailable,
                                 prepared: PreparedRoot { root,
                                                          permit } });
    };
    match interrupts::with_masked(|| manager.lock().roots.publish_prepared(permit, root))
    {
        Ok((publication, old)) =>
        {
            old.dispose();
            let (_, key) = publication.into_parts();
            Ok(RootBinding { key,
                             owner })
        }
        Err(failure) =>
        {
            Err(RootFailure { cause:    Error::Publication(failure.cause),
                              prepared: PreparedRoot { root:   failure.value,
                                                       permit: failure.permit } })
        }
    }
}
fn root(binding: &RootBinding, owner: &OwnerRef) -> Result<Arc<Root>, Error>
{
    if binding.owner != owner.owner()
    {
        return Err(Error::Authority);
    }
    let root =
        manager(|manager| manager.roots.completing(&binding.key).cloned())?.ok_or(Error::Missing)?;
    if root.owner.owner() != owner.owner()
    {
        return Err(Error::Authority);
    }
    Ok(root)
}
fn key(owner: &OwnerRef, id: u64) -> Result<KeyRead, Error>
{
    let key =
        KeyRead::new(manager(|manager| manager.keys.get(id).cloned())?.ok_or(Error::Missing)?);
    if key.position.root.owner.owner() != owner.owner()
    {
        return Err(Error::Authority);
    }
    Ok(key)
}
fn request_reap()
{
    REAP_PENDING.store(true, Ordering::Release);
}
/// Coalesced actual-change work at the guard-free maintenance boundary. Final
/// key readers only set the flag, so GET/SET and context release never scan the
/// list or destroy its code/account owners under their borrowed guards.
pub(crate) fn pump()
{
    if !REAP_PENDING.load(Ordering::Acquire)
    {
        return;
    }
    if REAP_RUNNING.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err()
    {
        // Another turn owns the detached list. It may still reinsert a node;
        // never consume that node's final-reader notification in this turn.
        return;
    }
    let _turn = ReapTurn;
    if !REAP_PENDING.swap(false, Ordering::AcqRel)
    {
        return;
    }
    let Some(manager) = MANAGER.get()
    else
    {
        return;
    };
    let mut list = interrupts::with_masked(|| manager.lock().deleted.take());
    while let Some(mut node) = list
    {
        list = node.next.take();
        if node.removal.as_ref().is_some_and(|removal| Arc::strong_count(&**removal) == 1)
        {
            drop(node);
        }
        else
        {
            interrupts::with_masked(|| {
                let mut manager = manager.lock();
                node.next = manager.deleted.take();
                manager.deleted = Some(node);
            });
        }
    }
}
pub(super) struct Progress
{
    owner: OwnerRef
}
impl Drop for Progress
{
    fn drop(&mut self)
    {
        if !self.owner.accepting()
        {
            crate::toolbox::processes::retirement::resources_released();
        }
    }
}
