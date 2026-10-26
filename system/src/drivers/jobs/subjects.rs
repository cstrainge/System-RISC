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

    pub(crate) fn admit(self: &Arc<Self>, authority: &Authority) -> Result<Subject, Status>
    {
        match authority
        {
            Authority::Operation(scope) =>
            {
                scope.admitting().map_err(|cause| {
                                      pool_status(mac_object_table::domain::PoolError::Scope {
                    owner: scope.subject(), kind: KindCode(*b"DIOP"), cause })
                                  })?;
                Ok(Subject { scope:      scope.clone(),
                             _retention: None })
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
                Ok(Subject { scope,
                             _retention: Some(retention) })
            }
        }
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
    _retention: Option<Arc<Retention>>
}
impl Subject
{
    pub(crate) fn scope(&self) -> &Lease
    {
        &self.scope
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
                  let kinds = crate::toolbox::handles::registry(|registry| {
                                  Ok((registry.get(KindCode(*b"DIOP"))?,
                                      registry.get(KindCode(*b"DIOB"))?))
                              }).map_err(|error| error.status())?
                                .map_err(|error| crate::toolbox::handles::registry_status(&error))?;
                  Operations::new(kinds)
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
