//! Registered document batches, retained through cancelled lower I/O and pin
//! cleanup. Ordinary IDs never stand in for completion acknowledgement.
#![forbid(unsafe_code)]
use alloc::vec::Vec;

use mac_abi::*;
use mac_object_table::{domain::ReleasedResource,
                       registry::{CleanupFailure, Release, Source, Specification},
                       store};

use super::Batch;
use crate::toolbox::handles::{CapacityPolicy, InvocationGate, KindCode, KindLease,
                              cleanup::Failure};

pub(in crate::toolbox::processes) struct Batches
{
    pub(super) cleanup_after:    u64,
    pub(super) cleanup_revision: u64,
    objects:                     Option<store::Store<Batch, InvocationGate>>,
    retiring:                    Vec<store::Removed<Batch, InvocationGate>>,
    completions:                 Vec<(u64, store::CompletionKey)>
}
impl Batches
{
    pub(in crate::toolbox::processes) const fn new() -> Self
    {
        Self { cleanup_after:    0,
               cleanup_revision: 0,
               objects:          None,
               retiring:         Vec::new(),
               completions:      Vec::new() }
    }

    pub(super) fn prepare(&mut self, kinds: (KindLease, KindLease)) -> Result<(), Status>
    {
        if self.objects.is_none()
        {
            self.objects = Some(store::Store::owned(kinds.0, kinds.1)
                .map_err(|error| crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error)))?);
        }
        Ok(())
    }

    pub(super) fn insert(&mut self, mut batch: Batch) -> Result<u64, Status>
    {
        let needed = self.completions.len().saturating_add(1);
        self.retiring
            .try_reserve(needed.saturating_sub(self.retiring.len()))
            .map_err(|_| NO_MEMORY)?;
        self.completions.try_reserve(1).map_err(|_| NO_MEMORY)?;
        let owner = batch.authority.clone();
        let objects = self.objects.as_mut().ok_or(NOT_SUPPORTED)?;
        let publication = objects.insert_with_receipt(&owner, |id| { batch.id = id; batch })
            .map_err(|error| crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error)))?;
        let (id, completion) = publication.into_parts();
        self.completions.push((id, completion));
        Ok(id)
    }

    pub(super) fn get(&self, id: u64) -> Option<&Batch>
    {
        self.objects.as_ref()?.get(id)
    }

    pub(super) fn get_mut(&mut self, id: u64) -> Option<&mut Batch>
    {
        self.objects.as_mut()?.get_mut(id)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (u64, &Batch)>
    {
        self.objects.iter().flat_map(|store| store.iter())
    }

    pub(super) fn completing(&self, id: u64) -> Option<&Batch>
    {
        self.retiring.iter().find_map(|mark| (mark.id == id).then_some(&**mark))
            .or_else(|| {
                let key = &self.completions.iter().find(|(handle, _)| *handle == id)?.1;
                self.objects.as_ref()?.completing(key)
            })
    }

    pub(super) fn retained(&self) -> impl Iterator<Item = (u64, &Batch)>
    {
        self.completions.iter().filter_map(|(id, _)| self.completing(*id).map(|batch| (*id, batch)))
    }

    pub(super) fn abandon(&mut self, id: u64) -> Result<(), Status>
    {
        self.retiring.try_reserve(1).map_err(|_| NO_MEMORY)?;
        let mark = self.objects.as_mut().ok_or(NOT_FOUND)?.remove(id).ok_or(NOT_FOUND)?;
        mark.abandon();
        self.retiring.push(mark);
        Ok(())
    }

    pub(super) fn finish(&mut self, id: u64)
    {
        let stopping = self.completing(id).is_some_and(|batch| !batch.authority.accepting());
        if let Some(batch) = self.completing(id)
        {
            batch.scope.close();
        }
        if let Some(at) = self.retiring.iter().position(|batch| batch.id == id)
        {
            drop(self.retiring.swap_remove(at));
            self.completions.retain(|(handle, _)| *handle != id);
        }
        else if let Some(objects) = &mut self.objects
        {
            if let Some(mark) = objects.remove(id)
            {
                drop(mark);
                self.completions.retain(|(handle, _)| *handle != id);
            }
            // Otherwise another dispatcher still owns the invalidation ticket.
            // Closed scope makes its retry ready; do not consume that ticket.
        }
        if stopping
        {
            crate::toolbox::processes::retirement::resources_released();
        }
    }

    fn release(&mut self, ticket: ReleasedResource, storage: bool) -> Result<(), Failure>
    {
        let Some(objects) = self.objects.as_mut()
        else
        {
            return Err(Failure::registration_mismatch(ticket));
        };
        if storage
        {
            return objects.release_storage(ticket).map_err(Into::into);
        }
        let pending = objects.releasing(&ticket).is_some_and(|batch| !batch.scope.closed());
        if pending && self.retiring.len() == self.retiring.capacity()
        {
            return Err(Failure::document_retirement_storage(ticket,
                                                            self.retiring.len(),
                                                            self.retiring.capacity()));
        }
        let mark = objects.release(ticket)?;
        let id = mark.id;
        if !mark.scope.closed()
        {
            mark.abandon();
            self.retiring.push(mark);
        }
        else
        {
            drop(mark);
            self.completions.retain(|(handle, _)| *handle != id);
        }
        Ok(())
    }
}
impl Drop for Batches
{
    fn drop(&mut self)
    {
        // Dropping a manager is not a provider cancellation acknowledgement.
        // Retain live pending batches too; Store's normal destruction alone
        // cannot prove their lower I/O has drained.
        loop
        {
            let next =
                self.objects.as_ref().and_then(|objects| objects.iter().next().map(|(id, _)| id));
            let Some(id) = next
            else
            {
                break;
            };
            let Some(mark) = self.objects.as_mut().and_then(|objects| objects.remove(id))
            else
            {
                break;
            };
            if mark.scope.closed()
            {
                drop(mark);
            }
            else if self.retiring.len() < self.retiring.capacity()
            {
                self.retiring.push(mark);
            }
            else
            {
                core::mem::forget(mark);
            }
        }
        if !self.retiring.is_empty()
        {
            core::mem::forget(core::mem::take(&mut self.retiring));
        }
    }
}
pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"DBST" } else { *b"DBAT" }),
                    label:    if storage
                    {
                        "Document batch backing"
                    }
                    else
                    {
                        "Document delivery batches"
                    },
                    provider: "System Process Manager",
                    source:   Source::Resident,
                    capacity: if storage
                    {
                        CapacityPolicy::HardBound { slots:  1,
                                                    reason: "One document-batch backing per domain" }
                    }
                    else
                    {
                        CapacityPolicy::GrowableUntilMemory
                    } }
}
pub(crate) struct ReleaseBatches(pub(crate) bool);
impl Release<crate::toolbox::runtime::Desktop, Failure> for ReleaseBatches
{
    fn release(&self, desktop: &mut crate::toolbox::runtime::Desktop, ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        desktop.processes.documents.release(ticket, self.0)
    }
}
