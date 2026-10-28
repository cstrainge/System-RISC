//! A real ledger root for a resident I/O operation. Delivery policy is separate
//! from its subject. The final owned use, not owner admission or a UI waiter,
//! releases the root and closes child admission.
#![forbid(unsafe_code)]
use alloc::sync::{Arc, Weak};
use core::sync::atomic::{AtomicU64, Ordering};

use mac_abi::{NO_MEMORY, Status};
use mac_object_table::{domain::ReleasedResource,
                       registry::{Source, Specification},
                       shared::InvocationGate as _};
use spinning_top::Spinlock;

use crate::toolbox::handles::{CapacityPolicy, InvocationGate, KindCode, KindLease, Store,
                              cleanup::Failure, subject::Authority};

type Lease = mac_object_table::scope::Lease<InvocationGate>;
type Removed = mac_object_table::store::Removed<Weak<Retention>, InvocationGate>;

pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"DIOB" } else { *b"DIOP" }),
                    label:    if storage
                    {
                        "Driver I/O operation storage"
                    }
                    else
                    {
                        "Driver I/O operations"
                    },
                    provider: "System I/O Manager",
                    capacity: if storage
                    {
                        CapacityPolicy::HardBound { slots:  1,
                                                    reason: "One I/O operation backing per \
                                                             subject domain" }
                    }
                    else
                    {
                        CapacityPolicy::GrowableUntilMemory
                    },
                    source:   Source::Resident }
}

pub(crate) struct Operations
{
    roots: Spinlock<Store<Weak<Retention>>>
}
impl Operations
{
    pub(crate) fn new(kinds: (KindLease, KindLease)) -> Result<Arc<Self>, Status>
    {
        let roots = Store::owned(kinds.0, kinds.1).map_err(pool_status)?;
        Arc::try_new(Self { roots: Spinlock::new(roots) }).map_err(|_| NO_MEMORY)
    }

    fn access<R>(&self, run: impl FnOnce(&mut Store<Weak<Retention>>) -> R) -> R
    {
        InvocationGate::run(|| run(&mut self.roots.lock()))
    }

    pub(crate) fn admit(self: &Arc<Self>, authority: &Authority,
                        allocation: crate::allocation::Target)
                        -> Result<Subject, Status>
    {
        let expected = match authority
        {
            Authority::Client(owner) => owner.owner(),
            Authority::Operation(scope) => scope.subject()
        };
        let actual = allocation.owner();
        if actual != expected
        {
            // Both capabilities are authentic, but pairing a foreign account
            // with this resource subject would charge/retain the wrong domain.
            return Err(mac_abi::ACCESS_DENIED);
        }
        crate::allocation::with_target(allocation.clone(), || {
            match authority
            {
                Authority::Operation(scope) =>
                {
                    scope.admitting().map_err(|cause| {
                                          pool_status(mac_object_table::domain::PoolError::Scope {
                    owner: scope.subject(), kind: KindCode(*b"DIOP"), cause })
                                      })?;
                    let allocation =
                        allocation.in_scope(scope)
                                  .map_err(|error| crate::allocation::account_status(&error))?;
                    Ok(Subject { scope: scope.clone(),
                                 _retention: None,
                                 request: None,
                                 allocation })
                }
                Authority::Client(owner) =>
                {
                    let reservation = mac_object_table::scope::Reservation::try_new(owner,
                    KindCode(*b"DIOP")).map_err(pool_status)?;
                    let scope = reservation.lease();
                    let retention =
                    Arc::try_new(Retention { operations: self.clone(),
                                             id:         AtomicU64::new(0),
                                             retiring:   Spinlock::new(None) }).map_err(|_| {
                                                                                   NO_MEMORY
                                                                               })?;
                    let weak = Arc::downgrade(&retention);
                    let id = self.access(|roots| roots.insert_root(reservation, weak))
                                 .map_err(pool_status)?;
                    retention.id.store(id, Ordering::Release);
                    let allocation =
                        allocation.in_scope(&scope)
                                  .map_err(|error| crate::allocation::account_status(&error))?;
                    Ok(Subject { scope,
                                 _retention: Some(retention),
                                 request: None,
                                 allocation })
                }
            }
        }).and_then(core::convert::identity)
    }

    /// The native adapter captures actual execution selection once. The core
    /// admission API accepts owned authentic authority explicitly on every
    /// platform; no owner label or missing-context System fallback is involved.
    pub(crate) fn admit_current(self: &Arc<Self>, authority: &Authority)
                                -> Result<Subject, Status>
    {
        let allocation = crate::allocation::capture_target()?;
        self.admit(authority, allocation)
    }

    pub(crate) fn release(&self, ticket: ReleasedResource, storage: bool) -> Result<(), Failure>
    {
        if storage
        {
            return self.access(|roots| roots.release_storage(ticket)).map_err(Failure::from);
        }
        let removed = self.access(|roots| roots.release(ticket)).map_err(Failure::from)?;
        // The payload holds a Weak, so this cannot make a cycle. The removal
        // guard, including the unique scope binding, outlives every real use.
        // A final use racing this upgrade either receives the guard or has
        // already finished; neither branch guesses an Arc reference count.
        if let Some(retention) = removed.upgrade()
        {
            InvocationGate::run(|| *retention.retiring.lock() = Some(removed));
        }
        Ok(())
    }
}

struct Retention
{
    operations: Arc<Operations>,
    id:         AtomicU64,
    retiring:   Spinlock<Option<Removed>>
}
impl Drop for Retention
{
    fn drop(&mut self)
    {
        // Destruction and acknowledgement run outside the manager lock. If the
        // walk owns a not-yet-dispatched ticket, it will find a dead Weak and
        // release it there; absence here never fabricates an acknowledgement.
        let removed = self.retiring.get_mut().take().or_else(|| {
            self.operations.access(|roots| roots.remove(self.id.load(Ordering::Acquire)))
        });
        drop(removed);
    }
}

#[derive(Clone)]
pub(crate) struct Subject
{
    scope:      Lease,
    _retention: Option<Arc<Retention>>,
    request:    Option<mac_object_table::store::CompletionKey>,
    // Last: the account-use pin outlives scope/Retention destruction and the
    // operation's actual cleanup acknowledgement, including delayed callbacks.
    allocation: crate::allocation::Target
}
impl Subject
{
    pub(crate) fn allocation(&self) -> &crate::allocation::Target
    {
        &self.allocation
    }

    pub(crate) fn scope(&self) -> &Lease
    {
        &self.scope
    }

    pub(crate) fn current_request(&self) -> Option<mac_object_table::store::CompletionKey>
    {
        self.request
    }

    /// The broker validates the receipt and active frame before installing
    /// this view. An invocation frame never creates a different resource root.
    pub(crate) fn in_request(&self, receipt: mac_object_table::store::CompletionKey) -> Self
    {
        let mut subject = self.clone();
        subject.request = Some(receipt);
        subject
    }

    /// A queue owns its request independently of the frame that submitted it.
    /// Its callback will install its own authenticated active frame.
    pub(crate) fn queued(&self) -> Self
    {
        let mut subject = self.clone();
        subject.request = None;
        subject
    }
}

fn pool_status(error: mac_object_table::domain::PoolError) -> Status
{
    crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error))
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn resident() -> Result<&'static Arc<Operations>, Status>
{
    static OPERATIONS: spin::Once<Arc<Operations>> = spin::Once::new();
    OPERATIONS.try_call_once(|| {
                  crate::allocator::with_system(crate::allocator::SystemClass::Ordinary, || {
                      let kinds =
                          crate::toolbox::handles::registry(|registry| {
                              Ok((registry.get(KindCode(*b"DIOP"))?,
                                  registry.get(KindCode(*b"DIOB"))?))
                          }).map_err(|error| error.status())?
                            .map_err(|error| crate::toolbox::handles::registry_status(&error))?;
                      Operations::new(kinds)
                  })
              })
}

#[cfg(target_arch = "riscv64")]
pub(crate) struct ReleaseOperations(pub bool);
#[cfg(target_arch = "riscv64")]
impl
    mac_object_table::registry::Release<dyn crate::toolbox::handles::cleanup::context::Context,
                                        Failure> for ReleaseOperations
{
    fn release(&self, _: &mut (dyn crate::toolbox::handles::cleanup::context::Context + 'static),
               ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        match resident()
        {
            Ok(operations) => operations.release(ticket, self.0),
            Err(_) => Err(Failure::context_unavailable(ticket, "I/O operation storage"))
        }
    }
}

#[cfg(test)]
mod tests;
