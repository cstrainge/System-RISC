//! Open-fork lifetimes. Invalidated provider marks retain their projection and
//! owner authority until Close acknowledges; ordinary IDs never reach that state.
#![forbid(unsafe_code)]
use mac_object_table::{domain::{CompletionError, ReleasedResource},
                       projected};

use super::*;
use crate::toolbox::handles::{InvocationGate, KindLease, OwnerRef};
pub(crate) mod lifetime;
type Retiring = projected::Removed<OpenFile, InvocationGate>;
pub(super) struct Marks
{
    objects:     Option<projected::Store<OpenFile, InvocationGate>>,
    retiring:    Vec<Retiring>,
    completions: Vec<(u32, projected::CompletionKey)>,
    #[cfg(test)]
    fixture:     Option<(crate::toolbox::handles::Registry<Manager>, Vec<(u64, OwnerRef)>)>
}
impl Marks
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
        if self.objects.is_none()
        {
            self.objects = Some(projected::Store::new(Kind::Files, kinds.0, kinds.1)
                .map_err(crate::toolbox::handles::projection_status)?);
        }
        Ok(())
    }

    pub(super) fn insert(&mut self, owner: &OwnerRef, file: OpenFile) -> Result<u32, Status>
    {
        let live = self.objects.as_ref().map_or(0, |objects| objects.iter().count());
        self.retiring.try_reserve(live + 1).map_err(|_| NO_MEMORY)?;
        self.completions.try_reserve(1).map_err(|_| NO_MEMORY)?;
        let objects = self.objects.as_mut().ok_or(NOT_SUPPORTED)?;
        let wire = objects.insert(owner, file).map_err(|error| {
                                                   #[cfg(target_arch = "riscv64")]
                                                   serial!("FILE_FORK_ADMISSION_REFUSED: \
                                                            owner={} {error}",
                                                           owner.owner());
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

    pub(super) fn get(&self, id: u32) -> Option<&OpenFile>
    {
        self.objects.as_ref()?.get(id)
    }

    pub(super) fn get_mut(&mut self, id: u32) -> Option<&mut OpenFile>
    {
        self.objects.as_mut()?.get_mut(id)
    }

    /// Resident completions alone use this path. The full wire generation
    /// stays reserved by the guard, so completion cannot hit a replacement.
    pub(super) fn completing(&self, id: u32) -> Option<&OpenFile>
    {
        self.get(id)
            .or_else(|| {
                self.objects
                    .as_ref()?
                    .completing(&self.completions.iter().find(|(wire, _)| *wire == id)?.1)
            })
            .or_else(|| self.retiring.iter().find(|mark| mark.wire() == id).map(|mark| &**mark))
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (u32, &OpenFile)>
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

    fn release(&mut self, ticket: ReleasedResource, storage: bool)
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
        let provider = objects.releasing(&ticket)
                              .is_some_and(|mark| matches!(&mark.fork, FileFork::Provider(fork) if !fork.closed()));
        if provider && self.retiring.len() == self.retiring.capacity()
        {
            return Err(crate::toolbox::handles::cleanup::Failure::fork_retirement_storage(ticket, self.retiring.len()));
        }
        let mark = objects.release(ticket)?;
        if let FileFork::Provider(fork) = &mark.fork
           && !fork.closed()
        {
            fork.abandon();
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
impl Drop for Marks
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
impl lifetime::ForkHost for Manager
{
    fn release_file_fork(&mut self, ticket: ReleasedResource, storage: bool)
                         -> Result<(), crate::toolbox::handles::cleanup::Failure>
    {
        self.handles.release(ticket, storage)
    }
}
#[cfg(test)]
impl Marks
{
    pub(super) fn fixture_owner(&mut self, label: u64) -> OwnerRef
    {
        use mac_object_table::domain::{DomainKind, OwnerIndex};
        if self.fixture.is_none()
        {
            let registrar =
                OwnerRef::try_new(OwnerIndex::issue(DomainKind::Module).unwrap()).unwrap();
            let mut registry = crate::toolbox::handles::Registry::<Manager>::new();
            let objects = registry.register(&registrar,
                                            lifetime::specification(false),
                                            lifetime::ReleaseForks)
                                  .unwrap();
            let storage = registry.register(&registrar,
                                            lifetime::specification(true),
                                            lifetime::ReleaseStorage)
                                  .unwrap();
            self.prepare((objects, storage)).unwrap();
            self.fixture = Some((registry, Vec::new()));
        }
        let owners = &mut self.fixture.as_mut().unwrap().1;
        if let Some((_, owner)) = owners.iter().find(|(alias, _)| *alias == label)
        {
            return owner.clone();
        }
        let owner = OwnerRef::try_new(OwnerIndex::issue(DomainKind::Process).unwrap()).unwrap();
        owners.push((label, owner.clone()));
        owner
    }
}

#[cfg(test)]
impl Manager
{
    pub(super) fn fixture_close_forks(&mut self, label: u64) -> usize
    {
        let ledger = self.handles.fixture_owner(label);
        let summary = ledger.summary();
        let count =
            (0..summary.kinds).map(|at| ledger.pool_at(summary.revision, at).unwrap())
                              .filter(|pool| {
                                  pool.kind == crate::toolbox::handles::KindCode(*b"FFRK")
                              })
                              .map(|pool| pool.live)
                              .sum();
        let walk =
            mac_object_table::walk::drain(self,
                                          &ledger,
                                          |manager, key| {
                                              manager.handles
                                                     .fixture
                                                     .as_ref()
                                                     .unwrap()
                                                     .0
                                                     .dispatch(key.kind())
                                          },
                                          |_, error| panic!("fixture cleanup refused: {error}"));
        assert!(walk.is_ok());
        count
    }
}

#[cfg(test)]
impl Manager
{
    pub(super) fn fixture_walk_forks(
        &mut self, label: u64, unavailable: bool, retry: bool)
        -> (mac_object_table::walk::Report, alloc::vec::Vec<alloc::string::String>)
    {
        let ledger = self.handles.fixture_owner(label);
        let resolve = |manager: &Self, key: mac_object_table::domain::ResourceKey| {
            if unavailable
            {
                Err(mac_object_table::registry::Error::Missing { kind: key.kind() })
            }
            else
            {
                manager.handles.fixture.as_ref().unwrap().0.dispatch(key.kind())
            }
        };
        let mut failures = Vec::new();
        let failed=|_,error:&mac_object_table::walk::Failure<mac_object_table::registry::Error,crate::toolbox::handles::cleanup::Cause>| { failures.push(alloc::format!("{error}")); };
        let report = if retry
                     {
                         mac_object_table::walk::retry(self, &ledger, resolve, failed)
                     }
                     else
                     {
                         mac_object_table::walk::drain(self, &ledger, resolve, failed)
                     }.unwrap();
        (report, failures)
    }
}
