//! Catalog references are process aliases of System-owned protection targets.
//! Generation checks and last-alias release live in the shared facility.
#![forbid(unsafe_code)]
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use mac_abi::*;
use mac_object_table::{aliases,
                       domain::ReleasedResource,
                       registry::{CleanupFailure, Release, Source, Specification}};

use crate::toolbox::handles::{CapacityPolicy, InvocationGate, KindCode, KindLease, OwnerRef,
                              cleanup::Failure};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct File
{
    pub(super) volume: u32,
    pub(super) id:     u32
}
pub(super) struct Pins
{
    aliases: Option<aliases::Store<File, InvocationGate, Role>>
}
impl Pins
{
    pub(super) const fn new() -> Self
    {
        Self { aliases: None }
    }

    pub(super) fn prepare(&mut self, system: OwnerRef, kinds: [KindLease; 4])
                          -> Result<(), Status>
    {
        if self.aliases.is_none()
        {
            let [alias, backing, object, object_backing] = kinds;
            self.aliases = Some(aliases::Store::new(system,
                                                    (alias, backing),
                                                    (object, object_backing)).map_err(status)?);
        }
        Ok(())
    }

    pub(super) fn insert(&mut self, owner: &OwnerRef, volume: u32, id: u32) -> Result<(), Status>
    {
        let store = self.aliases.as_mut().ok_or(NOT_SUPPORTED)?;
        if store.iter().any(|(_, actual, _, role)| {
                           actual == owner.owner() && matches!(role, Role::Executable)
                       })
        {
            return Err(BAD_ABI);
        }
        store.insert(owner,
                     File { volume,
                            id },
                     Role::Executable)
             .map(|_| ())
             .map_err(status)
    }

    pub(super) fn document(&mut self, owner: &OwnerRef, batch: u64, scope: ScopeLease,
                           volume: u32, id: u32)
                           -> Result<(), Status>
    {
        if scope.owner() != owner.owner() || scope.closed()
        {
            return Err(ACCESS_DENIED);
        }
        let store = self.aliases.as_mut().ok_or(NOT_SUPPORTED)?;
        if store.iter().any(|(_, actual, file, role)| {
                           actual == owner.owner()
                           && file.volume == volume
                           && file.id == id
                           && role.batch() == Some(batch)
                       })
        {
            return Ok(());
        }
        store.insert(owner,
                     File { volume,
                            id },
                     Role::Document { batch,
                                      scope })
             .map(|_| ())
             .map_err(status)
    }

    pub(super) fn unpin_documents(&mut self, batch: u64)
    {
        if let Some(store) = &mut self.aliases
        {
            store.remove_where(|_, role| role.batch() == Some(batch));
            store.finish_deferred(|role| {
                     match role
                     {
                         Role::Executable => true,
                         Role::Document { scope,
                                          .. } => scope.closed()
                     }
                 });
        }
    }

    /// Publish a complete receiver set before removing the sender's aliases.
    /// The closure is resident queue bookkeeping only, never storage or code
    /// supplied by a client. All allocation precedes its publication point.
    pub(super) fn handoff(&mut self, sender: &OwnerRef, old: u64, receiver: &OwnerRef, new: u64,
                          scope: ScopeLease, publish: impl FnOnce() -> Result<(), Status>)
                          -> Result<(), Status>
    {
        if scope.owner() != receiver.owner() || scope.closed()
        {
            return Err(ACCESS_DENIED);
        }
        let store = self.aliases.as_mut().ok_or(NOT_SUPPORTED)?;
        let count =
            store.iter()
                 .filter(|(_, owner, _, role)| {
                     *owner == sender.owner() && role.batch() == Some(old)
                 })
                 .count();
        if count == 0
        {
            return Err(NOT_FOUND);
        }
        let mut source = alloc::vec::Vec::new();
        let mut created = alloc::vec::Vec::new();
        source.try_reserve_exact(count).map_err(|_| NO_MEMORY)?;
        created.try_reserve_exact(count).map_err(|_| NO_MEMORY)?;
        source.extend(store.iter().filter_map(|(id, owner, _, role)|
            (owner == sender.owner() && role.batch() == Some(old)).then_some(id)));
        let result = (|| {
            for id in source
            {
                created.push(store.share(sender,
                                         id,
                                         receiver,
                                         Role::Document { batch: new,
                                                          scope: scope.clone() })
                                  .map_err(status)?);
            }
            publish()
        })();
        if result.is_err()
        {
            // A concurrent owner walk may already own a new alias's ticket;
            // ordinary removal never acknowledges that other capability.
            for id in created
            {
                let _ = store.remove(receiver, id);
            }
        }
        else
        {
            store.remove_where(|owner, role| owner == sender.owner() && role.batch() == Some(old));
        }
        result
    }

    pub(super) fn objects(&self) -> impl Iterator<Item = &File>
    {
        self.aliases.iter().flat_map(|store| store.objects())
    }

    pub(super) fn len(&self) -> usize
    {
        self.objects().count()
    }
}
fn status(error: aliases::Error) -> Status
{
    match error
    {
        aliases::Error::Pool(error) => crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error)),
        aliases::Error::Missing { .. } | aliases::Error::TargetGone { .. } => NOT_FOUND,
        aliases::Error::Foreign { .. } => ACCESS_DENIED,
        aliases::Error::ReferenceLimit { .. } => NO_MEMORY,
        aliases::Error::NotSystem { .. } => BAD_ABI
    }
}
#[derive(Clone, Copy)]
pub(crate) enum Registration
{
    Alias,
    Backing,
    Object,
    ObjectBacking
}
impl Registration
{
    pub(crate) const ALL: [Self; 4] =
        [Self::Alias, Self::Backing, Self::Object, Self::ObjectBacking];

    pub(crate) fn specification(self) -> Specification<'static>
    {
        let (kind, label) = match self
        {
            Self::Alias => (*b"FPIN", "Catalog aliases"),
            Self::Backing => (*b"FPST", "Catalog alias backing"),
            Self::Object => (*b"FPOB", "Shared catalog pins"),
            Self::ObjectBacking => (*b"FPOS", "Shared catalog backing")
        };
        Specification { kind: KindCode(kind),
                        label,
                        provider: "System File Manager",
                        capacity: CapacityPolicy::GrowableUntilMemory,
                        source: Source::Resident }
    }
}
pub(crate) trait PinHost
{
    fn release_file_pin(&mut self, ticket: ReleasedResource, kind: Registration)
                        -> Result<(), Failure>;
}
impl<C: PinHost> Release<C, Failure> for Registration
{
    fn release(&self, context: &mut C, ticket: ReleasedResource) -> Result<(), Failure>
    {
        context.release_file_pin(ticket, *self)
    }
}
impl PinHost for super::Manager
{
    fn release_file_pin(&mut self, ticket: ReleasedResource, kind: Registration)
                        -> Result<(), Failure>
    {
        let Some(store) = self.pins.aliases.as_mut()
        else
        {
            return Err(Failure::registration_mismatch(ticket));
        };
        match kind
        {
            Registration::Alias =>
            {
                if let Some((_,
                             Role::Document { scope,
                                              .. })) = store.releasing(&ticket)
                {
                    if !scope.closed()
                    {
                        return store.defer_alias(ticket).map_err(Into::into);
                    }
                }
                store.release_alias(ticket)
            }
            Registration::Backing => store.release_alias_storage(ticket),
            Registration::Object => store.release_object(ticket),
            Registration::ObjectBacking => store.release_object_storage(ticket)
        }.map_err(Into::into)
    }
}
impl super::Manager
{
    pub(crate) fn prepare_executable_pins(&mut self, system: OwnerRef, kinds: [KindLease; 4])
                                          -> Result<(), Status>
    {
        self.pins.prepare(system, kinds)
    }
}


/// One non-cloneable owner closes a protocol scope; borrowed alias leases can
/// observe closure but cannot close or reopen it. Neither type issues IDs.
pub(crate) struct ScopeOwner
{
    state: Arc<ScopeState>
}
struct ScopeState
{
    owner:  mac_object_table::domain::DomainId,
    closed: AtomicBool
}
#[derive(Clone)]
pub(crate) struct ScopeLease
{
    state: Arc<ScopeState>
}
impl ScopeOwner
{
    pub(crate) fn new(owner: &OwnerRef) -> Result<Self, Status>
    {
        if !owner.accepting()
        {
            return Err(SERVICE_BUSY);
        }
        Ok(Self { state:
                      Arc::try_new(ScopeState { owner:  owner.owner(),
                                                closed: AtomicBool::new(false) }).map_err(|_| {
                                                                                     NO_MEMORY
                                                                                 })? })
    }

    pub(crate) fn lease(&self) -> ScopeLease
    {
        ScopeLease { state: self.state.clone() }
    }

    pub(crate) fn close(&self)
    {
        self.state.closed.store(true, Ordering::Release);
    }

    pub(crate) fn closed(&self) -> bool
    {
        self.state.closed.load(Ordering::Acquire)
    }
}
impl Drop for ScopeOwner
{
    fn drop(&mut self)
    {
        self.close();
    }
}
impl ScopeLease
{
    fn owner(&self) -> mac_object_table::domain::DomainId
    {
        self.state.owner
    }

    fn closed(&self) -> bool
    {
        self.state.closed.load(Ordering::Acquire)
    }
}
enum Role
{
    Executable,
    Document
    {
        batch: u64,
        scope: ScopeLease
    }
}
impl Role
{
    fn batch(&self) -> Option<u64>
    {
        match self
        {
            Self::Executable => None,
            Self::Document { batch,
                             .. } => Some(*batch)
        }
    }
}
