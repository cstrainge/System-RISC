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
    fn access_allowed(&self, access: super::subject::Access<'_>) -> bool;
    fn scope(&self) -> Option<&super::subject::Lease>
    {
        None
    }
    fn scope_retains(&self) -> bool
    {
        self.scope().is_some_and(|scope| scope.admitting().is_ok())
    }
}
struct Retiring<T>
{
    mark:          projected::Removed<T, InvocationGate>,
    waiting_scope: bool
}
impl<T> core::ops::Deref for Retiring<T>
{
    type Target = T;

    fn deref(&self) -> &T
    {
        &self.mark
    }
}
impl<T> Retiring<T>
{
    fn wire(&self) -> u32
    {
        self.mark.wire()
    }
}

/// Each resident kind observes the common close clock independently. Polling
/// an unchanged clock never scans objects or invokes cleanup.
pub(super) struct ScopeChanges(core::sync::atomic::AtomicU64);
impl ScopeChanges
{
    pub(super) const fn new() -> Self
    {
        Self(core::sync::atomic::AtomicU64::new(0))
    }

    pub(super) fn pending(&self) -> bool
    {
        self.0.load(core::sync::atomic::Ordering::Acquire)
        != mac_object_table::scope::closed_revision()
    }

    pub(super) fn observe<T: Payload>(&self, objects: &mut Objects<T>)
    {
        let observed = objects.refresh_scopes();
        self.0.store(observed, core::sync::atomic::Ordering::Release);
    }
}

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
        self.admit(|objects| objects.insert(owner, file))
    }

    pub(super) fn insert_authorized(&mut self, authority: &super::subject::Authority, file: T)
                                    -> Result<u32, Status>
    {
        self.admit(|objects| {
                match authority
                {
                    super::subject::Authority::Client(owner) => objects.insert(owner, file),
                    super::subject::Authority::Operation(scope) =>
                    {
                        objects.insert_in_scope(scope, file)
                    }
                }
            })
    }

    pub(super) fn insert_root(&mut self, reservation: super::subject::Root,
                              make: impl FnOnce(u32) -> T)
                              -> Result<u32, Status>
    {
        self.admit(|objects| objects.insert_root(reservation, make))
    }

    fn admit(&mut self,
             create: impl FnOnce(&mut projected::Store<T, InvocationGate>)
                    -> Result<u32, projected::Error>)
             -> Result<u32, Status>
    {
        // Completion entries include live, pending and retired payloads. Their
        // count bounds all later removals without a full object-value scan.
        let needed = self.completions.len().saturating_add(1);
        self.retiring
            .try_reserve(needed.saturating_sub(self.retiring.len()))
            .map_err(|_| NO_MEMORY)?;
        self.completions.try_reserve(1).map_err(|_| NO_MEMORY)?;
        let objects = self.objects.as_mut().ok_or(NOT_SUPPORTED)?;
        let wire = create(objects).map_err(|error| {
                                      #[cfg(target_arch = "riscv64")]
                                      serial!("FILE_OBJECT_ADMISSION_REFUSED: kind={} {error}",
                                              T::KIND);
                                      crate::toolbox::handles::projection_status(error)
                                  })?;
        let Some(key) = objects.completion_key(wire)
        else
        {
            // The owner walk may have won invalidation after publication; its
            // real ticket still owns cleanup through the registered store.
            drop(objects.remove(wire));
            return Err(SERVICE_BUSY);
        };
        self.completions.push((wire, key));
        Ok(wire)
    }

    pub(super) fn get(&self, id: u32) -> Option<&T>
    {
        self.objects.as_ref()?.get(id)
    }

    pub(super) fn get_for(&self, access: super::subject::Access<'_>, id: u32)
                          -> Result<&T, Status>
    {
        let object = match access
                     {
                         super::subject::Access::Client(_) => self.get(id),
                         super::subject::Access::Operation(_) => self.completing(id)
                     }.ok_or(NOT_FOUND)?;
        if !object.access_allowed(access)
        {
            return Err(ACCESS_DENIED);
        }
        Ok(object)
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

    /// Explicit nonblocking disposal invalidates before scheduling storage
    /// cleanup. Reserve first: refusal leaves the ordinary object untouched.
    pub(super) fn abandon(&mut self, id: u32) -> Result<(), Status>
    {
        let pending = self.get(id).ok_or(NOT_FOUND)?.cleanup_pending();
        if pending
        {
            self.retiring.try_reserve(1).map_err(|_| NO_MEMORY)?;
        }
        let mark = self.objects.as_mut().ok_or(NOT_FOUND)?.remove(id).ok_or(NOT_FOUND)?;
        if pending
        {
            mark.begin_cleanup();
            self.retiring.push(Retiring { mark,
                                          waiting_scope: false });
        }
        else
        {
            drop(mark);
            self.forget_completion(id);
        }
        Ok(())
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

    /// Called on a real scope-close change. Existing lower I/O still owns
    /// its ACK condition; this only starts cleanup deferred behind its root.
    pub(super) fn refresh_scopes(&mut self) -> u64
    {
        let revision = mac_object_table::scope::closed_revision();
        let mut at = 0;
        while at < self.retiring.len()
        {
            if self.retiring[at].waiting_scope && !self.retiring[at].scope_retains()
            {
                self.retiring[at].waiting_scope = false;
                self.retiring[at].begin_cleanup();
                if !self.retiring[at].cleanup_pending()
                {
                    let mark = self.retiring.swap_remove(at);
                    let wire = mark.wire();
                    let stopping = mark.owner_stopping();
                    drop(mark);
                    self.forget_completion(wire);
                    #[cfg(target_arch = "riscv64")]
                    if stopping
                    {
                        crate::toolbox::processes::retirement::resources_released();
                    }
                    #[cfg(not(target_arch = "riscv64"))]
                    let _ = stopping;
                    continue;
                }
            }
            at += 1;
        }
        revision
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
        let provider =
            objects.releasing(&ticket)
                   .is_some_and(|object| object.cleanup_pending() || object.scope_retains());
        if provider && self.retiring.len() == self.retiring.capacity()
        {
            return Err(crate::toolbox::handles::cleanup::Failure::file_retirement_storage(ticket, self.retiring.len(), self.retiring.capacity()));
        }
        let mark = objects.release(ticket)?;
        let waiting_scope = mark.scope_retains();
        if waiting_scope || mark.cleanup_pending()
        {
            if !waiting_scope
            {
                mark.begin_cleanup();
            }
            self.retiring.push(Retiring { mark,
                                          waiting_scope });
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
