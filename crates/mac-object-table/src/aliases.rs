//! Ledger-owned aliases of System-domain objects. Both identities are issued
//! by the facility; a caller never supplies a target key or changes an owner.
//! All operations borrow the store, so a resolved payload cannot outlive the
//! alias validation without holding that borrow. Destructors run outside locks.
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
    }
}
impl fmt::Display for ReleaseCause
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Facility(error) => error.fmt(f),
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
    aliases: store::Store<Alias<S>, G>,
    objects: store::Store<Object<T>, G>,
    system:  OwnerRef<G>
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
        Ok(Self { aliases: store::Store::owned(aliases.0, aliases.1).map_err(Error::Pool)?,
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
        let existing =
            self.objects.iter().find_map(|(id, object)| (object.value == value).then_some(id));
        let (id, fresh) = if let Some(id) = existing
        {
            (id, false)
        }
        else
        {
            (self.objects
                 .insert(&self.system,
                         Object { value,
                                  references: AtomicUsize::new(0) })
                 .map_err(Error::Pool)?,
             true)
        };
        let Some(completion) = self.objects.completion_key(id)
        else
        {
            // Exclusive insertion produced this key; fail closed if admission
            // was revoked before publication. No alias has been published.
            drop(self.objects.remove(id));
            return Err(Error::TargetGone { alias: 0 });
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
        if !self.system.accepting()
        {
            return Err(Error::Pool(PoolError::Stopping { owner: self.system.owner() }));
        }
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
        // A target may itself be quarantined by the System-domain walk. Its
        // opaque completion key permits ref release, never ordinary access.
        if let Some(object) = self.objects.completing(&target.completion)
        {
            let count = object.references.load(Ordering::Acquire);
            if count != 0
            {
                object.references.store(count - 1, Ordering::Release);
                if count == 1
                {
                    drop(self.objects.remove(target.id));
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
        loop
        {
            let next = self.aliases.iter().next().map(|(id, _)| id);
            let Some(id) = next
            else
            {
                break;
            };
            let Some(removed) = self.aliases.remove(id)
            else
            {
                break;
            };
            self.forget(removed.target);
            drop(removed);
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
