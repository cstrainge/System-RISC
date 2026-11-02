//! FFRK's connected use of the shared canonical prepared treaty publisher.
#![forbid(unsafe_code)]
use mac_object_table::{CapacityError, domain::ReleasedResource, projected,
                       registry::CleanupFailure, store};

use super::*;

pub(in crate::toolbox::files) type Plan = projected::PublicationPlan<OpenFile, InvocationGate>;
pub(in crate::toolbox::files) type Preparation =
    projected::PreparedPublication<OpenFile, InvocationGate>;
pub(in crate::toolbox::files) type Permit = projected::PublicationPermit<InvocationGate>;
pub(in crate::toolbox::files) type Retired = projected::RetiredCapacity<OpenFile, InvocationGate>;
pub(in crate::toolbox::files) type PublicationRetired =
    projected::RetiredPublication<InvocationGate>;
pub(in crate::toolbox::files) type Backing = projected::RetiredOwner<OpenFile, InvocationGate>;
#[derive(Clone, Copy, Debug)]
pub(in crate::toolbox::files) enum Error
{
    Index(Status),
    Facility(projected::PreparedError)
}
impl Error
{
    pub(in crate::toolbox::files) fn status(self) -> Status
    {
        use projected::PreparedError as P;
        match self
        {
            Self::Index(status) => status,
            Self::Facility(P::Native(store::PreparedError::Pool(cause))) =>
            {
                crate::toolbox::handles::projection_status(projected::Error::Pool(cause))
            }
            Self::Facility(P::Native(store::PreparedError::CapacityChanged)
                           | P::Projection(CapacityError::CapacityChanged)) => SERVICE_BUSY,
            Self::Facility(P::Projection(CapacityError::AllocationFailed
                                         | CapacityError::Exhausted)) => NO_MEMORY,
            Self::Facility(P::Native(store::PreparedError::WrongStore
                                     | store::PreparedError::RetiredStore)
                           | P::Projection(CapacityError::WrongKind)) => BAD_ABI
        }
    }
}
impl Objects<OpenFile>
{
    pub(in crate::toolbox::files) fn plan_fork(&mut self, owner: &OwnerRef) -> Result<Plan, Error>
    {
        self.reserve().map_err(Error::Index)?;
        self.objects
            .as_ref()
            .ok_or(Error::Index(NOT_SUPPORTED))?
            .0
            .plan_publication(owner)
            .map_err(Error::Facility)
    }

    pub(in crate::toolbox::files) fn admit_fork(&mut self, input: &mut Option<Preparation>,
                                                retired: &mut Option<Retired>)
                                                -> Result<Permit, Error>
    {
        // Include every real outstanding credit, not a mirrored per-owner
        // count. Legacy publishers preserve the same spare before insertion.
        self.reserve().map_err(Error::Index)?;
        let objects = self.objects.as_mut().ok_or(Error::Index(NOT_SUPPORTED))?;
        let preparation = input.take().ok_or(Error::Index(BAD_ABI))?;
        match objects.0.admit_prepared(preparation)
        {
            Ok((permit, old)) =>
            {
                *retired = Some(old);
                Ok(permit)
            }
            Err(failure) =>
            {
                *input = Some(failure.preparation);
                Err(Error::Facility(failure.cause))
            }
        }
    }

    pub(in crate::toolbox::files) fn admit_fork_wire(&mut self, permit: &mut Permit)
                                                     -> Result<u32, Error>
    {
        self.objects
            .as_mut()
            .ok_or(Error::Index(NOT_SUPPORTED))?
            .0
            .admit_projection(permit)
            .map_err(Error::Facility)
    }

    pub(in crate::toolbox::files) fn admit_fork_capacity(&mut self, permit: &Permit,
                                                         capacity:&mut projected::PreparedCapacity<InvocationGate>)
                                                         -> Result<(), Error>
    {
        self.objects
            .as_mut()
            .ok_or(Error::Index(NOT_SUPPORTED))?
            .0
            .admit_capacity(permit, capacity)
            .map_err(Error::Facility)
    }

    pub(in crate::toolbox::files) fn publish_fork(&mut self,
                                                  input: &mut Option<(Permit, OpenFile)>,
                                                  retired: &mut Option<PublicationRetired>)
                                                  -> Result<u32, Error>
    {
        // This invariant is enforced before consuming the original permit or
        // payload; there is no allocation or lost committed outcome on refusal.
        if self.completions.len() == self.completions.capacity()
           || self.retiring.capacity() < self.completions.len() + 1
        {
            return Err(Error::Index(BAD_ABI));
        }
        let objects = self.objects.as_mut().ok_or(Error::Index(NOT_SUPPORTED))?;
        let (permit, value) = input.take().ok_or(Error::Index(BAD_ABI))?;
        match objects.0.publish_prepared(permit, value)
        {
            Ok((publication, old)) =>
            {
                let (id, key) = publication.into_parts();
                self.completions.push((id, key));
                *retired = Some(old);
                Ok(id)
            }
            Err(failure) =>
            {
                *input = Some((failure.permit, failure.value));
                Err(Error::Facility(failure.cause))
            }
        }
    }

    pub(in crate::toolbox::files) fn release_fork_owned(
        &mut self, ticket: ReleasedResource, storage: bool)
        -> Result<Option<Backing>, crate::toolbox::handles::cleanup::Failure>
    {
        if !storage
        {
            return self.release(ticket, false).map(|()| None);
        }
        let Some(objects) = self.objects.as_mut()
        else
        {
            return Err(crate::toolbox::handles::cleanup::Failure::registration_mismatch(ticket));
        };
        let preparing = objects.0.publication_preparations(ticket.key().owner());
        if preparing != 0
        {
            return Err(crate::toolbox::handles::cleanup::Failure::publication_preparations(ticket,preparing));
        }
        objects.0.release_storage_owned(ticket).map_err(Into::into)
    }
}
