//! Registered open-fork adapter over shared file-object storage.
#![forbid(unsafe_code)]
use mac_object_table::domain::ReleasedResource;

use super::*;
pub(crate) mod lifetime;
impl super::objects::Payload for OpenFile
{
    type Storage = crate::toolbox::files::objects::storage::Treaty<Self>;

    const KIND: crate::toolbox::handles::KindCode = crate::toolbox::handles::KindCode(*b"FFRK");
    const WIRE: Option<Kind> = Some(Kind::Files);

    #[cfg(systemrisc)]
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
#[cfg(systemrisc)]
#[must_use = "dispose open-fork backing after the Files guard ends"]
pub(crate) struct ForkRelease(Option<objects::prepared::Backing>);
#[cfg(systemrisc)]
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
#[cfg(systemrisc)]
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


