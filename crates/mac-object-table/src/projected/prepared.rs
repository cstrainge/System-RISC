//! Detached capacity and original canonical publication for treaty IDs.
//! The wire index never owns payloads or substitutes for native completion.
use alloc::sync::Arc;
use core::fmt;

use super::{Entry, Projection, ProjectionRelease, Publication, Store};
use crate::{CapacityError, Kind,
            domain::{DomainId, PoolError},
            registry::KindLease,
            shared::{InvocationGate, OwnerRef},
            store};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreparedError
{
    Native(store::PreparedError),
    Projection(CapacityError)
}
impl From<store::PreparedError> for PreparedError
{
    fn from(cause: store::PreparedError) -> Self
    {
        Self::Native(cause)
    }
}
impl From<PoolError> for PreparedError
{
    fn from(cause: PoolError) -> Self
    {
        Self::Native(cause.into())
    }
}
impl From<CapacityError> for PreparedError
{
    fn from(cause: CapacityError) -> Self
    {
        Self::Projection(cause)
    }
}
impl fmt::Display for PreparedError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Native(cause) => cause.fmt(out),
            Self::Projection(cause) => cause.fmt(out)
        }
    }
}
impl core::error::Error for PreparedError
{
}
#[must_use]
pub struct AdmissionFailure<P>
{
    pub cause:       PreparedError,
    pub preparation: P
}
#[must_use]
pub struct PublishFailure<P, T>
{
    pub cause:  PreparedError,
    pub permit: P,
    pub value:  T
}

pub struct PublicationPlan<T, G: InvocationGate>
{
    native:     store::PublicationPlan<Entry<T>, G>,
    projection: Arc<Projection>,
    capacity:   crate::CapacityPlan<u64>
}
#[must_use]
pub struct PreparedPublication<T, G: InvocationGate>
{
    native:   store::PreparedPublication<Entry<T>, G>,
    capacity: PreparedCapacity<G>
}
#[must_use]
pub struct PreparedCapacity<G: InvocationGate>
{
    projection: Arc<Projection>,
    capacity:   crate::PreparedCapacity<u64>,
    gate:       core::marker::PhantomData<G>
}
#[must_use]
pub struct RetiredCapacity<T, G: InvocationGate>
{
    native:   store::RetiredCapacity<Entry<T>, G>,
    capacity: PreparedCapacity<G>
}
impl<T, G: InvocationGate> RetiredCapacity<T, G>
{
    pub fn dispose(self)
    {
        let Self { native,
                   capacity } = self;
        drop((native, capacity));
    }
}
/// Original canonical permit; a selected treaty identity is never reissued
/// after a later native refusal. An attached release belongs to its completion
/// reserve, which follows the actual native object through removal and ACK.
#[must_use]
pub struct PublicationPermit<G: InvocationGate>
{
    native:     store::PublicationPermit<G>,
    projection: Arc<Projection>,
    wire:       Option<u32>,
    unattached: Option<ProjectionRelease>
}
#[must_use]
pub struct RetiredPublication<G: InvocationGate>
{
    native:     store::RetiredPublication<G>,
    projection: Arc<Projection>
}
impl<G: InvocationGate> RetiredPublication<G>
{
    pub fn dispose(self)
    {
        let Self { native,
                   projection } = self;
        drop((native, projection));
    }
}
impl<T, G: InvocationGate> PublicationPlan<T, G>
{
    /// Outside all metadata guards, under the original recipient's allocation
    /// authority. Only the projection vector uses System allocation selection.
    pub fn prepare(self) -> Result<PreparedPublication<T, G>, PreparedError>
    {
        let capacity = G::with_system_allocation(|| self.capacity.prepare())?;
        let native = self.native.prepare()?;
        Ok(PreparedPublication { native,
                                 capacity: PreparedCapacity { projection: self.projection,
                                                              capacity,
                                                              gate: core::marker::PhantomData } })
    }
}
impl<T, G: InvocationGate> PreparedPublication<T, G>
{
    /// Refresh only projection capacity outside manager guards. A native
    /// CapacityChanged refusal instead requires a fresh pre-permit native plan.
    pub fn prepare_capacity(&self) -> Result<PreparedCapacity<G>, PreparedError>
    {
        let plan = self.capacity.projection.access::<G, _>(|table| table.plan_capacity())?;
        let capacity = G::with_system_allocation(|| plan.prepare())?;
        Ok(PreparedCapacity { projection: self.capacity.projection.clone(),
                              capacity,
                              gate: core::marker::PhantomData })
    }

    /// The swapped-out allocation remains with the caller for guard-free drop.
    pub fn replace_capacity(&mut self, capacity: &mut PreparedCapacity<G>)
                            -> Result<(), PreparedError>
    {
        if !Arc::ptr_eq(&self.capacity.projection, &capacity.projection)
        {
            return Err(store::PreparedError::WrongStore.into());
        }
        core::mem::swap(&mut self.capacity.capacity, &mut capacity.capacity);
        Ok(())
    }
}
impl<G: InvocationGate> PublicationPermit<G>
{
    pub fn id(&self) -> u64
    {
        self.native.id()
    }

    /// Capacity replanning does not consume another canonical identity. Call
    /// outside manager guards; only the bounded snapshot takes the table gate.
    pub fn prepare_capacity(&self) -> Result<PreparedCapacity<G>, PreparedError>
    {
        let plan = self.projection.access::<G, _>(|table| table.plan_capacity())?;
        let capacity = G::with_system_allocation(|| plan.prepare())?;
        Ok(PreparedCapacity { projection: self.projection.clone(),
                              capacity,
                              gate: core::marker::PhantomData })
    }
}
impl<T, G: InvocationGate> Store<T, G>
{
    pub fn new_prepared(wire: Kind, payload: KindLease<G>, backing: KindLease<G>)
                        -> Result<Self, PreparedError>
    {
        let projection = G::with_system_allocation(|| {
                             let entries = spinning_top::Spinlock::new(crate::Table::new(wire));
                             Arc::try_new(Projection { entries })
                         }).map_err(|_| CapacityError::AllocationFailed)?;
        let values = store::Store::owned_prepared(payload, backing)?;
        Ok(Self { projection,
                  values })
    }

    pub fn publication_preparations(&self, owner: DomainId) -> usize
    {
        self.values.publication_preparations(owner)
    }

    pub fn plan_publication(&self, owner: &OwnerRef<G>)
                            -> Result<PublicationPlan<T, G>, PreparedError>
    {
        let capacity = self.projection.access::<G, _>(|table| table.plan_capacity())?;
        let native = self.values.plan_publication(owner)?;
        Ok(PublicationPlan { native,
                             capacity,
                             projection: self.projection.clone() })
    }

    /// Refusal retains every original owner, including capacity installed before
    /// native admission refused. Drop returned owners outside metadata guards.
    pub fn admit_prepared(
        &mut self, mut preparation: PreparedPublication<T, G>)
        -> Result<(PublicationPermit<G>, RetiredCapacity<T, G>),
                  AdmissionFailure<PreparedPublication<T, G>>>
    {
        if !Arc::ptr_eq(&self.projection, &preparation.capacity.projection)
        {
            return Err(AdmissionFailure { cause: store::PreparedError::WrongStore.into(),
                                          preparation });
        }
        if let Err(cause) =
            self.projection
                .access::<G, _>(|table| table.admit_capacity(&mut preparation.capacity.capacity))
        {
            return Err(AdmissionFailure { cause: cause.into(),
                                          preparation });
        }
        match self.values.admit_prepared(preparation.native)
        {
            Ok((native, retired)) =>
            {
                Ok((PublicationPermit { native,
                                        projection: self.projection.clone(),
                                        wire: None,
                                        unattached: None },
                    RetiredCapacity { native:   retired,
                                      capacity: preparation.capacity }))
            }
            Err(failure) =>
            {
                Err(AdmissionFailure { cause:       failure.cause.into(),
                                       preparation: PreparedPublication { native:
                                                                              failure.preparation,
                                                                          capacity:
                                                                              preparation.capacity } })
            }
        }
    }

    /// Installs detached index capacity only; no native Store operation occurs
    /// under the projection lock. The old empty allocation remains in capacity.
    pub fn admit_capacity(&self, permit: &PublicationPermit<G>, capacity: &mut PreparedCapacity<G>)
                          -> Result<(), PreparedError>
    {
        if !Arc::ptr_eq(&self.projection, &permit.projection)
           || !Arc::ptr_eq(&self.projection, &capacity.projection)
        {
            return Err(store::PreparedError::WrongStore.into());
        }
        self.values.validate_prepared(&permit.native)?;
        self.projection.access::<G, _>(|table| table.admit_capacity(&mut capacity.capacity))?;
        Ok(())
    }

    /// Admit the actual treaty slot before a caller performs external effects.
    /// The returned number is observational: it grants no payload lookup or
    /// completion authority and does not promise that the owner cannot stop.
    /// Refusal retains the same borrowed permit, including any selected wire
    /// and original completion hook. Replenish capacity outside guards and retry
    /// this permit; never issue a replacement native identity for this operation.
    pub fn admit_projection(&self, permit: &mut PublicationPermit<G>)
                            -> Result<u32, PreparedError>
    {
        if !Arc::ptr_eq(&self.projection, &permit.projection)
        {
            return Err(store::PreparedError::WrongStore.into());
        }
        self.values.validate_prepared(&permit.native)?;
        let wire = match permit.wire
        {
            Some(wire) =>
            {
                let valid = self.projection.access::<G, _>(|table| table.get(wire).copied())
                            == Some(permit.id());
                if !valid
                {
                    return Err(PoolError::InvalidGrowth.into());
                }
                wire
            }
            None =>
            {
                let native_id = permit.id();
                let selected =
                    self.projection.access::<G, _>(|table| table.insert_prepared(native_id));
                match selected
                {
                    Ok(wire) =>
                    {
                        permit.wire = Some(wire);
                        permit.unattached =
                            Some(ProjectionRelease::new::<G>(self.projection.clone(), wire));
                        wire
                    }
                    Err((cause, _)) => return Err(cause.into())
                }
            }
        };
        if let Some(release) = permit.unattached.take()
        {
            if let Err(release) = permit.native.attach_projection(release)
            {
                permit.unattached = Some(release);
                return Err(PoolError::InvalidGrowth.into());
            }
        }
        Ok(wire)
    }

    pub fn publish_prepared(
        &mut self, mut permit: PublicationPermit<G>, value: T)
        -> Result<(Publication, RetiredPublication<G>), PublishFailure<PublicationPermit<G>, T>>
    {
        let wire = match self.admit_projection(&mut permit)
        {
            Ok(wire) => wire,
            Err(cause) =>
            {
                return Err(PublishFailure { cause,
                                            permit,
                                            value });
            }
        };
        match self.values.publish_prepared(permit.native,
                                           Entry { wire,
                                                   value })
        {
            Ok((native, retired)) =>
            {
                Ok((Publication::new(wire, native),
                    RetiredPublication { native:     retired,
                                         projection: permit.projection }))
            }
            Err(failure) =>
            {
                Err(PublishFailure { cause:  failure.cause.into(),
                                     permit: PublicationPermit { native:     failure.permit,
                                                                 projection: permit.projection,
                                                                 wire:       Some(wire),
                                                                 unattached: permit.unattached },
                                     value:  failure.value.value })
            }
        }
    }
}
