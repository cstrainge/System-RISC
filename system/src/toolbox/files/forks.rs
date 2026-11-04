//! Registered open-fork adapter over shared file-object storage.
#![forbid(unsafe_code)]
use mac_object_table::domain::ReleasedResource;

use super::*;
#[cfg(test)]
use crate::toolbox::handles::OwnerRef;
pub(crate) mod lifetime;
impl super::objects::Payload for OpenFile
{
    type Storage = crate::toolbox::files::objects::storage::Treaty<Self>;

    const KIND: crate::toolbox::handles::KindCode = crate::toolbox::handles::KindCode(*b"FFRK");
    const WIRE: Option<Kind> = Some(Kind::Files);

    #[cfg(guest)]
    fn io_subject(&self) -> Option<&crate::drivers::jobs::subjects::Subject>
    {
        self.io_subject.as_ref()
    }

    fn cleanup_pending(&self) -> bool
    {
        self.operations.load(core::sync::atomic::Ordering::Acquire) != 0
        || (self.final_handle()
            && matches!(&*self.description.fork.lock(),FileFork::Provider(fork) if !fork.closed()))
    }

    fn begin_cleanup(&self)
    {
        self.relinquish();
        if self.last_close.load(core::sync::atomic::Ordering::Acquire)
           && let FileFork::Provider(fork) = &*self.description.fork.lock()
        {
            fork.abandon();
        }
    }

    fn access_allowed(&self, access: super::subject::Access<'_>) -> bool
    {
        self.authority.matches(self.owner, access)
    }

    fn owner_stopping(&self) -> bool
    {
        OpenFile::owner_stopping(self)
    }

    fn scope(&self) -> Option<&super::subject::Lease>
    {
        self.authority.scope()
    }
}
#[cfg(guest)]
#[must_use = "dispose open-fork backing after the Files guard ends"]
pub(crate) struct ForkRelease(Option<objects::prepared::Backing>);
#[cfg(guest)]
impl ForkRelease
{
    pub(crate) fn dispose(self)
    {
        if let Some(backing) = self.0
        {
            backing.dispose();
        }
    }
}
#[cfg(guest)]
impl Manager
{
    pub(crate) fn release_fork_owned(
        &mut self, ticket: ReleasedResource, storage: bool)
        -> Result<ForkRelease, crate::toolbox::handles::cleanup::Failure>
    {
        self.handles.release_fork_owned(ticket, storage).map(ForkRelease)
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
impl super::objects::Objects<OpenFile>
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
    pub(super) fn fixture_close_objects(&mut self, label: u64) -> usize
    {
        let ledger = self.handles.fixture_owner(label);
        let summary = ledger.summary();
        let count = (0..summary.kinds).map(|at| ledger.pool_at(summary.revision, at).unwrap())
                                      .filter(|pool| {
                                          [crate::toolbox::handles::KindCode(*b"FFRK"),
                                   crate::toolbox::handles::KindCode(*b"FDWR"),
                                   crate::toolbox::handles::KindCode(*b"FSWR"),
                                   crate::toolbox::handles::KindCode(*b"FIGR")].contains(&pool.kind)
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
    pub(super) fn fixture_walk_files(
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
