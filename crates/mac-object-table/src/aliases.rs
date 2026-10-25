//! Ledger-owned aliases of System-domain objects. Both identities are issued
//! by the facility; a caller never supplies a target key or changes an owner.
//! All operations borrow the store, so a resolved payload cannot outlive the
//! alias validation without holding that borrow. Destructors run outside locks.
use alloc::vec::Vec;
use core::{fmt,
           sync::atomic::{AtomicUsize, Ordering}};

use crate::{domain::{CompletionError, DomainId, DomainKind, PoolError, ReleasedResource},
            registry::{CleanupFailure, KindLease},
            shared::{InvocationGate, OwnerRef},
            store};

#[derive(Debug)]
pub enum Error
{
    Pool(PoolError),
    Missing
    {
        alias: u64
    },
    Foreign
    {
        alias:    u64,
        expected: DomainId,
        actual:   DomainId
    },
    TargetGone
    {
        alias: u64
    },
    ReferenceLimit
    {
        alias: u64
    },
    NotSystem
    {
        owner: DomainId
    }
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Pool(error) => error.fmt(f),
            Self::Missing { alias } => write!(f, "alias {alias} is absent, stale, or invalidated"),
            Self::Foreign { alias,
                            expected,
                            actual } =>
            {
                write!(f, "alias {alias} belongs to {actual}, not requesting {expected}")
            }
            Self::TargetGone { alias } =>
            {
                write!(f, "alias {alias}'s System object was invalidated; reacquire the object")
            }
            Self::ReferenceLimit { alias } =>
            {
                write!(f, "alias {alias}'s object has exhausted its reference counter")
            }
            Self::NotSystem { owner } =>
            {
                write!(f, "shared objects require the System ledger, received {owner}")
            }
        }
    }
}
impl core::error::Error for Error
{
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseCause
{
    Facility(PoolError),
    Referenced
    {
        aliases: usize
    },
    RetirementStorage
    {
        pending:  usize,
        reserved: usize
    }
}
impl fmt::Display for ReleaseCause
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Facility(error) => error.fmt(f),
            Self::RetirementStorage { pending,
                                      reserved } =>
            {
                write!(f,
                       "alias retirement storage is full ({pending} pending, {reserved} \
                        reserved); unique cleanup ticket remains owned for retry")
            }
            Self::Referenced { aliases } =>
            {
                write!(f,
                       "shared object still has {aliases} aliases; payload and cleanup ticket \
                        remain retained for retry")
            }
        }
    }
}
#[derive(Debug)]
pub struct ReleaseError
{
    ticket: ReleasedResource,
    cause:  ReleaseCause
}
impl fmt::Display for ReleaseError
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        self.cause.fmt(f)
    }
}
impl core::error::Error for ReleaseError
{
}
impl From<CompletionError> for ReleaseError
{
    fn from(error: CompletionError) -> Self
    {
        Self { cause:  ReleaseCause::Facility(error.cause()),
               ticket: error.into_ticket() }
    }
}
impl CleanupFailure for ReleaseError
{
    type Cause = ReleaseCause;

    fn into_parts(self) -> (ReleasedResource, ReleaseCause)
    {
        (self.ticket, self.cause)
    }

    fn registration_mismatch(ticket: ReleasedResource) -> Self
    {
        CompletionError::registration_mismatch(ticket).into()
    }
}

struct Object<T>
{
    value:      T,
    references: AtomicUsize
}
#[derive(Clone, Copy)]
struct Target
{
    id:         u64,
    completion: store::CompletionKey
}
struct Alias<S>
{
    target: Target,
    tag:    S
}

/// The four registrations separate process aliases/backing from global
/// objects/backing. Policy and release authority come from checked leases.
pub struct Store<T, G: InvocationGate, S = ()>
{
    alias_kind: crate::domain::KindCode,
    deferred:   Vec<store::Removed<Alias<S>, G>>,
    aliases:    store::Store<Alias<S>, G>,
    objects:    store::Store<Object<T>, G>,
    system:     OwnerRef<G>
}
impl<T, G: InvocationGate, S> Store<T, G, S>
{
    pub fn new(system: OwnerRef<G>, aliases: (KindLease<G>, KindLease<G>),
               objects: (KindLease<G>, KindLease<G>))
               -> Result<Self, Error>
    {
        if system.owner().kind() != DomainKind::System
        {
            return Err(Error::NotSystem { owner: system.owner() });
        }
        // All four namespaces must differ: a pool has exactly one store.
        let kinds = [aliases.0.registration().code,
                     aliases.1.registration().code,
                     objects.0.registration().code,
                     objects.1.registration().code];
        for at in 0..kinds.len()
        {
            if kinds[..at].contains(&kinds[at])
            {
                return Err(Error::Pool(PoolError::DuplicateKind { owner: system.owner(),
                                                                  kind:  kinds[at] }));
            }
        }
        Ok(Self { alias_kind: kinds[0],
                  deferred: Vec::new(),
                  aliases: store::Store::owned(aliases.0, aliases.1).map_err(Error::Pool)?,
                  objects: store::Store::owned(objects.0, objects.1).map_err(Error::Pool)?,
                  system })
    }

    fn owned(&self, owner: &OwnerRef<G>, id: u64) -> Result<&Alias<S>, Error>
    {
        let (actual, alias) = self.aliases.get_with_owner(id).ok_or(Error::Missing { alias: id })?;
        if actual != owner.owner()
        {
            return Err(Error::Foreign { alias: id,
                                        expected: owner.owner(),
                                        actual });
        }
        Ok(alias)
    }

    pub fn get(&self, owner: &OwnerRef<G>, id: u64) -> Result<(&T, &S), Error>
    {
        let alias = self.owned(owner, id)?;
        let object = self.objects.get(alias.target.id).ok_or(Error::TargetGone { alias: id })?;
        Ok((&object.value, &alias.tag))
    }

    /// Intern an immutable target, charging only the alias to its process. An
    /// existing target is shared; admission failure never leaves an extra ref.
    pub fn insert(&mut self, owner: &OwnerRef<G>, value: T, tag: S) -> Result<u64, Error>
        where T: PartialEq
    {
        if !owner.accepting()
        {
            return Err(Error::Pool(PoolError::Stopping { owner: owner.owner() }));
        }
        let existing =
            self.objects.iter().find_map(|(id, object)| (object.value == value).then_some(id));
        let (id, completion, fresh) = if let Some(id) = existing
        {
            let completion =
                self.objects.completion_key(id).ok_or(Error::TargetGone { alias: 0 })?;
            (id, completion, false)
        }
        else
        {
            let publication = self.objects
                                  .insert_with_receipt(&self.system, |_| {
                                      Object { value,
                                               references: AtomicUsize::new(0) }
                                  })
                                  .map_err(Error::Pool)?;
            let (id, completion) = publication.into_parts();
            (id, completion, true)
        };
        let result = self.admit(owner,
                                Target { id,
                                         completion },
                                tag);
        if fresh && result.is_err()
        {
            drop(self.objects.remove(id));
        }
        result
    }

    fn admit(&mut self, owner: &OwnerRef<G>, target: Target, tag: S) -> Result<u64, Error>
    {
        if !owner.accepting()
        {
            return Err(Error::Pool(PoolError::Stopping { owner: owner.owner() }));
        }
        if !self.system.accepting()
        {
            return Err(Error::Pool(PoolError::Stopping { owner: self.system.owner() }));
        }
        let live = self.aliases.stored_len();
        self.deferred.try_reserve(live.saturating_add(1)).map_err(|_| {
                                                              Error::Pool(PoolError::Storage {
            owner: owner.owner(), kind: self.alias_kind, used: live,
            requested_slots: live.saturating_add(1), limit: None })
                                                          })?;
        let object = self.objects.get(target.id).ok_or(Error::TargetGone { alias: 0 })?;
        let count = object.references.load(Ordering::Acquire);
        let next = count.checked_add(1).ok_or(Error::ReferenceLimit { alias: 0 })?;
        let id = self.aliases
                     .insert(owner,
                             Alias { target,
                                     tag })
                     .map_err(Error::Pool)?;
        object.references.store(next, Ordering::Release);
        Ok(id)
    }

    /// Genuine source and recipient ledgers are required. The original alias
    /// remains unchanged if the recipient is stopping or out of capacity.
    pub fn share(&mut self, source: &OwnerRef<G>, id: u64, recipient: &OwnerRef<G>, tag: S)
                 -> Result<u64, Error>
    {
        let target = self.owned(source, id)?.target;
        self.admit(recipient, target, tag)
    }

    pub fn iter(&self) -> impl Iterator<Item = (u64, DomainId, &T, &S)>
    {
        self.aliases.iter().filter_map(|(id, alias)| {
                               let owner = self.aliases.owner(id)?;
                               let value = &self.objects.get(alias.target.id)?.value;
                               Some((id, owner, value, &alias.tag))
                           })
    }

    /// Includes targets held by invalidated aliases awaiting a real cleanup
    /// callback. Mutation/unmount protection must not enumerate live aliases.
    pub fn objects(&self) -> impl Iterator<Item = &T>
    {
        self.objects.iter().map(|(_, object)| &object.value)
    }

    fn forget(&mut self, target: Target)
    {
        Self::forget_target(&mut self.objects, target);
    }

    fn forget_target(objects: &mut store::Store<Object<T>, G>, target: Target)
    {
        // A target may itself be quarantined by the System-domain walk. Its
        // opaque completion key permits ref release, never ordinary access.
        if let Some(object) = objects.completing(&target.completion)
        {
            let count = object.references.load(Ordering::Acquire);
            if count != 0
            {
                object.references.store(count - 1, Ordering::Release);
                if count == 1
                {
                    drop(objects.remove(target.id));
                }
            }
        }
    }

    pub fn remove(&mut self, owner: &OwnerRef<G>, id: u64) -> Result<(), Error>
    {
        let target = self.owned(owner, id)?.target;
        let removed = self.aliases.remove(id).ok_or(Error::Missing { alias: id })?;
        self.forget(target);
        drop(removed);
        Ok(())
    }

    pub fn release_alias(&mut self, ticket: ReleasedResource) -> Result<(), ReleaseError>
    {
        let removed = self.aliases.release(ticket)?;
        let target = removed.target;
        self.forget(target);
        drop(removed);
        Ok(())
    }

    /// Retain an invalidated alias until a real protocol acknowledgement.
    /// Capacity is reserved before publication; no allocation is needed here.
    pub fn defer_alias(&mut self, ticket: ReleasedResource) -> Result<(), ReleaseError>
    {
        if self.aliases.releasing(&ticket).is_none()
        {
            return Err(ReleaseError::registration_mismatch(ticket));
        }
        if self.deferred.len() == self.deferred.capacity()
        {
            return Err(ReleaseError { ticket,
                                      cause: ReleaseCause::RetirementStorage { pending:
                                                                                   self.deferred
                                                                                       .len(),
                                                                               reserved:
                                                                                   self.deferred
                                                                                       .capacity() } });
        }
        self.deferred.push(self.aliases.release(ticket)?);
        Ok(())
    }

    /// Called only when the manager's protocol state changes. Each retained
    /// alias still owns its unique native removal guard and target reference.
    pub fn finish_deferred(&mut self, mut ready: impl FnMut(&S) -> bool) -> usize
    {
        let mut at = 0;
        let mut released = 0;
        while at < self.deferred.len()
        {
            if ready(&self.deferred[at].tag)
            {
                let mark = self.deferred.swap_remove(at);
                self.forget(mark.target);
                drop(mark);
                released += 1;
            }
            else
            {
                at += 1;
            }
        }
        released
    }

    /// Cleanup inspects a retained alias only through its unique invalidate-
    /// first ticket. A target's completion key is private to the alias store.
    pub fn releasing(&self, ticket: &ReleasedResource) -> Option<(&T, &S)>
    {
        let alias = self.aliases.releasing(ticket)?;
        let object = self.objects.completing(&alias.target.completion)?;
        Some((&object.value, &alias.tag))
    }

    /// Trusted manager bookkeeping, without allocating an ID array. Each
    /// removal still consumes the facility's matching live alias identity.
    pub fn remove_where(&mut self, mut predicate: impl FnMut(DomainId, &S) -> bool)
    {
        let objects = &mut self.objects;
        self.aliases.remove_where_with(|owner, alias| predicate(owner, &alias.tag),
                                       |removed| {
                                           Self::forget_target(objects, removed.target);
                                           drop(removed);
                                       });
    }

    pub fn release_object(&mut self, ticket: ReleasedResource) -> Result<(), ReleaseError>
    {
        if let Some(object) = self.objects.releasing(&ticket)
        {
            let aliases = object.references.load(Ordering::Acquire);
            if aliases != 0
            {
                return Err(ReleaseError { ticket,
                                          cause: ReleaseCause::Referenced { aliases } });
            }
        }
        drop(self.objects.release(ticket)?);
        Ok(())
    }

    pub fn release_alias_storage(&mut self, ticket: ReleasedResource) -> Result<(), ReleaseError>
    {
        self.aliases.release_storage(ticket).map_err(Into::into)
    }

    pub fn release_object_storage(&mut self, ticket: ReleasedResource) -> Result<(), ReleaseError>
    {
        self.objects.release_storage(ticket).map_err(Into::into)
    }
}
impl<T, G: InvocationGate, S> Drop for Store<T, G, S>
{
    fn drop(&mut self)
    {
        // Drain ordinary aliases with their ref accounting. Invalidated ones
        // still belong to a dispatcher; destruction is not its acknowledgement.
        self.remove_where(|_, _| true);
        // Destruction is not a protocol acknowledgement. Guards and their
        // target references remain pending if the owning manager disappears.
        if !self.deferred.is_empty()
        {
            core::mem::forget(core::mem::take(&mut self.deferred));
        }

        loop
        {
            let next = self.objects.iter().find_map(|(id, object)|
                (object.references.load(Ordering::Acquire) != 0).then_some(id));
            let Some(id) = next
            else
            {
                break;
            };
            let Some(ticket) = self.objects.begin_release(id)
            else
            {
                break;
            };
            if let Err(error) = self.objects.quarantine(ticket)
            {
                // Preserve the capability even if this damaged store cannot
                // attach it. The ledger's pending barrier stays closed.
                core::mem::forget(error);
            }
        }
    }
}
