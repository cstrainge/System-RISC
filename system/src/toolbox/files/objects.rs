//! Registered file-object storage and retained completion authority. Payload
//! adapters own cleanup semantics; IDs and release tickets stay in the facility.
#![forbid(unsafe_code)]
use mac_object_table::{domain::{CompletionError, ReleasedResource},
                       projected};

use super::*;
use crate::toolbox::handles::{InvocationGate, KindLease, OwnerRef};
pub(super) trait Payload
{
    const WIRE: Kind;
    const KIND: crate::toolbox::handles::KindCode;
    fn cleanup_pending(&self) -> bool;
    fn begin_cleanup(&self);
    fn owner_stopping(&self) -> bool;
}
type Retiring<T> = projected::Removed<T, InvocationGate>;
pub(super) struct Objects<T: Payload>
{
    objects:            Option<projected::Store<T, InvocationGate>>,
    retiring:           Vec<Retiring<T>>,
    completions:        Vec<(u32, projected::CompletionKey)>,
    #[cfg(test)]
    pub(super) fixture: Option<(crate::toolbox::handles::Registry<Manager>, Vec<(u64, OwnerRef)>)>
}
impl<T: Payload> Objects<T>
{
    pub(super) const fn new() -> Self
    {
        Self { objects:              None,
               retiring:             Vec::new(),
               completions:          Vec::new(),
               #[cfg(test)]
               fixture:              None }
    }

    pub(crate) fn prepare(&mut self, kinds: (KindLease, KindLease)) -> Result<(), Status>
    {
        if kinds.0.registration().code != T::KIND
        {
            return Err(BAD_ABI);
        }
        if self.objects.is_none()
        {
            self.objects = Some(projected::Store::new(T::WIRE, kinds.0, kinds.1)
                .map_err(crate::toolbox::handles::projection_status)?);
        }
        Ok(())
    }

    pub(super) fn insert(&mut self, owner: &OwnerRef, file: T) -> Result<u32, Status>
    {
        let live = self.objects.as_ref().map_or(0, |objects| objects.iter().count());
        self.retiring.try_reserve(live + 1).map_err(|_| NO_MEMORY)?;
        self.completions.try_reserve(1).map_err(|_| NO_MEMORY)?;
        let objects = self.objects.as_mut().ok_or(NOT_SUPPORTED)?;
        let wire = objects.insert(owner, file).map_err(|error| {
                                                   #[cfg(target_arch = "riscv64")]
                                                   serial!("FILE_OBJECT_ADMISSION_REFUSED: \
                                                            owner={} kind={} {error}",
                                                           owner.owner(),
                                                           T::KIND);
                                                   crate::toolbox::handles::projection_status(error)
                                               })?;
        let Some(key) = objects.completion_key(wire)
        else
        {
            drop(objects.remove(wire));
            return Err(BAD_ABI);
        };
        self.completions.push((wire, key));
        Ok(wire)
    }

    pub(super) fn get(&self, id: u32) -> Option<&T>
    {
        self.objects.as_ref()?.get(id)
    }

    pub(super) fn get_mut(&mut self, id: u32) -> Option<&mut T>
    {
        self.objects.as_mut()?.get_mut(id)
    }

    /// Resident completions alone use this path. The full wire generation
    /// stays reserved by the guard, so completion cannot hit a replacement.
    pub(super) fn completing(&self, id: u32) -> Option<&T>
    {
        self.get(id)
            .or_else(|| {
                self.objects
                    .as_ref()?
                    .completing(&self.completions.iter().find(|(wire, _)| *wire == id)?.1)
            })
            .or_else(|| self.retiring.iter().find(|mark| mark.wire() == id).map(|mark| &**mark))
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (u32, &T)>
    {
        // Mutation/unmount protection includes invalidated payloads whose
        // dispatcher was refused as well as guards awaiting external ACK.
        self.completions.iter().filter_map(|(wire, key)| {
                                   let file = self.objects
                                                  .as_ref()
                                                  .and_then(|objects| objects.completing(key))
                                                  .or_else(|| {
                                                      self.retiring
                                                          .iter()
                                                          .find(|mark| mark.wire() == *wire)
                                                          .map(|mark| &**mark)
                                                  })?;
                                   Some((*wire, file))
                               })
    }

    pub(super) fn cleanup_pending(&self) -> bool
    {
        !self.retiring.is_empty()
        || self.completions.iter().any(|(wire, _)| self.get(*wire).is_none())
    }

    pub(super) fn len(&self) -> usize
    {
        self.iter().count()
    }

    fn forget_completion(&mut self, id: u32)
    {
        if let Some(at) = self.completions.iter().position(|(wire, _)| *wire == id)
        {
            self.completions.swap_remove(at);
        }
    }

    pub(super) fn reserve(&mut self) -> Result<(), Status>
    {
        self.completions.try_reserve(1).map_err(|_| NO_MEMORY)?;
        self.retiring.try_reserve(self.len() + 1).map_err(|_| NO_MEMORY)
    }

    pub(super) fn remove(&mut self, id: u32) -> Option<()>
    {
        if let Some(index) = self.retiring.iter().position(|mark| mark.wire() == id)
        {
            let owner_stopping = self.retiring[index].owner_stopping();
            drop(self.retiring.swap_remove(index));
            self.forget_completion(id);
            #[cfg(target_arch = "riscv64")]
            if owner_stopping
            {
                crate::toolbox::processes::retirement::resources_released();
            }
            #[cfg(not(target_arch = "riscv64"))]
            let _ = owner_stopping;
            return Some(());
        }
        if let Some(mark) = self.objects.as_mut()?.remove(id)
        {
            drop(mark);
            self.forget_completion(id);
        }
        else
        {
            // A ticket may still be quarantined before its dispatcher ran.
            // The callback records terminal state, then the common retry owns
            // destruction; this acknowledgement cannot consume its ticket.
            let key = &self.completions.iter().find(|(wire, _)| *wire == id)?.1;
            let file = self.objects.as_ref()?.completing(key)?;
            #[cfg(target_arch = "riscv64")]
            if file.owner_stopping()
            {
                crate::toolbox::processes::retirement::resources_released();
            }
            #[cfg(not(target_arch = "riscv64"))]
            let _ = file;
        }
        Some(())
    }

    pub(super) fn release(&mut self, ticket: ReleasedResource, storage: bool)
                          -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        let Some(objects) = self.objects.as_mut()
        else
        {
            return Err(<CompletionError as mac_object_table::registry::CleanupFailure>::registration_mismatch(ticket).into());
        };
        if storage
        {
            return objects.release_storage(ticket).map_err(Into::into);
        }
        let provider = objects.releasing(&ticket).is_some_and(Payload::cleanup_pending);
        if provider && self.retiring.len() == self.retiring.capacity()
        {
            return Err(crate::toolbox::handles::cleanup::Failure::file_retirement_storage(ticket, self.retiring.len(), self.retiring.capacity()));
        }
        let mark = objects.release(ticket)?;
        if mark.cleanup_pending()
        {
            mark.begin_cleanup();
            self.retiring.push(mark);
        }
        else
        {
            let wire = mark.wire();
            drop(mark);
            self.forget_completion(wire);
        }
        Ok(())
    }
}
impl<T: Payload> Drop for Objects<T>
{
    fn drop(&mut self)
    {
        // Manager destruction is not an external provider acknowledgement.
        if !self.retiring.is_empty()
        {
            core::mem::forget(core::mem::take(&mut self.retiring));
        }
    }
}
