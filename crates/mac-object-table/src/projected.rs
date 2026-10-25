//! Existing 32-bit treaty namespaces projected onto registered owner storage.
//! The projection is an index, never a second payload or ownership authority.
use alloc::sync::Arc;
use core::{fmt, ops::Deref};

use spinning_top::Spinlock;

use crate::{Kind, Table,
            domain::{CompletionError, DomainId, PoolError, ReleasedResource},
            registry::KindLease,
            shared::{InvocationGate, OwnerRef},
            store};

#[derive(Debug)]
pub enum Error
{
    ProjectionExhausted,
    Pool(PoolError)
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::ProjectionExhausted =>
            {
                f.write_str("32-bit object projection has no reusable slot or backing memory")
            }
            Self::Pool(error) => error.fmt(f)
        }
    }
}
impl core::error::Error for Error
{
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)>
    {
        match self
        {
            Self::Pool(error) => Some(error),
            _ => None
        }
    }
}

struct Entry<T>
{
    wire:  u32,
    value: T
}
struct Projection
{
    entries: Spinlock<Table<u64>>
}
impl Projection
{
    fn access<G: InvocationGate, R>(&self, operation: impl FnOnce(&mut Table<u64>) -> R) -> R
    {
        // Guards can outlive their store. The architecture gate still encloses
        // every access; no payload destructor or callback runs under this lock.
        G::run(|| operation(&mut self.entries.lock()))
    }
}
// Kept in the native completion reservation until the matching ACK succeeds.
// The retained owner ledger can then finish the treaty slot even if its store
// or original removal guard has gone away. No allocation on the failure path.
pub(crate) struct ProjectionRelease
{
    projection: Arc<Projection>,
    wire:       u32,
    release:    fn(&Projection, u32)
}
impl ProjectionRelease
{
    fn new<G: InvocationGate>(projection: Arc<Projection>, wire: u32) -> Self
    {
        fn release<G: InvocationGate>(projection: &Projection, wire: u32)
        {
            projection.access::<G, _>(|index| index.remove(wire));
        }
        Self { projection,
               wire,
               release: release::<G> }
    }
}
impl Drop for ProjectionRelease
{
    fn drop(&mut self)
    {
        (self.release)(&self.projection, self.wire);
    }
}

/// Opaque completion authority, minted before invalidation. It never validates
/// an ordinary treaty ID and does not acknowledge an external operation.
#[derive(Clone, Copy)]
pub struct CompletionKey
{
    native: store::CompletionKey,
    wire:   u32
}

/// Treaty identity and native completion provenance captured by the same
/// publication. Wire bits grant no late authority after ordinary invalidation.
#[derive(Clone, Copy)]
pub struct Publication
{
    completion: CompletionKey
}
impl Publication
{
    pub fn id(self) -> u32
    {
        self.completion.wire
    }

    pub fn into_parts(self) -> (u32, CompletionKey)
    {
        (self.completion.wire, self.completion)
    }

    fn new(wire: u32, native: store::Publication) -> Self
    {
        let (_, native) = native.into_parts();
        Self { completion: CompletionKey { native,
                                           wire } }
    }
}

pub struct Store<T, G: InvocationGate>
{
    projection: Arc<Projection>,
    values:     store::Store<Entry<T>, G>
}
impl<T, G: InvocationGate> Store<T, G>
{
    pub fn new(wire: Kind, payload: KindLease<G>, backing: KindLease<G>) -> Result<Self, Error>
    {
        Ok(Self { projection: Arc::try_new(Projection { entries: Spinlock::new(Table::new(wire)) })
                                  .map_err(|_| Error::ProjectionExhausted)?,
                  values:     store::Store::owned(payload, backing).map_err(Error::Pool)? })
    }

    pub fn insert(&mut self, owner: &OwnerRef<G>, value: T) -> Result<u32, Error>
    {
        self.insert_with_receipt(owner, value).map(Publication::id)
    }

    pub fn insert_with_receipt(&mut self, owner: &OwnerRef<G>, value: T)
                               -> Result<Publication, Error>
    {
        if !owner.accepting()
        {
            return Err(Error::Pool(PoolError::Stopping { owner: owner.owner() }));
        }
        // No partially admitted object can be reached through an ordinary ID.
        // Generation history is consumed even when later publication refuses.
        let wire = self.projection
                       .access::<G, _>(|index| index.insert(0))
                       .map_err(|_| Error::ProjectionExhausted)?;
        let projection = ProjectionRelease::new::<G>(self.projection.clone(), wire);
        let native = self.values
                         .insert_projected(owner,
                                           Entry { wire,
                                                   value },
                                           projection)
                         .map_err(Error::Pool)?;
        self.projection
            .access::<G, _>(|index| {
                *index.get_mut(wire).expect("exclusive provisional projection") = native.id();
            });
        Ok(Publication::new(wire, native))
    }

    pub fn insert_root(&mut self, reservation: crate::scope::Reservation<G>,
                       make: impl FnOnce(u32) -> T)
                       -> Result<u32, Error>
    {
        self.insert_root_with_receipt(reservation, make).map(Publication::id)
    }

    pub fn insert_root_with_receipt(&mut self, reservation: crate::scope::Reservation<G>,
                                    make: impl FnOnce(u32) -> T)
                                    -> Result<Publication, Error>
    {
        let wire = self.projection
                       .access::<G, _>(|index| index.insert(0))
                       .map_err(|_| Error::ProjectionExhausted)?;
        let projection = ProjectionRelease::new::<G>(self.projection.clone(), wire);
        let native = self.values
                         .insert_root_value(reservation,
                                            |_| {
                                                Entry { wire,
                                                        value: make(wire) }
                                            },
                                            Some(projection))
                         .map_err(Error::Pool)?;
        self.projection.access::<G, _>(|index| {
                           *index.get_mut(wire).expect("exclusive unpublished root projection \
                                                        is retained until native publication") =
                               native.id();
                       });
        Ok(Publication::new(wire, native))
    }

    pub fn insert_in_scope(&mut self, scope: &crate::scope::Lease<G>, value: T)
                           -> Result<u32, Error>
    {
        self.insert_in_scope_with_receipt(scope, value).map(Publication::id)
    }

    pub fn insert_in_scope_with_receipt(&mut self, scope: &crate::scope::Lease<G>, value: T)
                                        -> Result<Publication, Error>
    {
        scope.admitting().map_err(|cause| {
                              Error::Pool(PoolError::Scope { owner: scope.subject(),
                                                             kind: self.values.kind(),
                                                             cause })
                          })?;
        let wire = self.projection
                       .access::<G, _>(|index| index.insert(0))
                       .map_err(|_| Error::ProjectionExhausted)?;
        let projection = ProjectionRelease::new::<G>(self.projection.clone(), wire);
        let native = self.values
                         .insert_scoped_value(scope,
                                              |_| {
                                                  Entry { wire,
                                                          value }
                                              },
                                              Some(projection))
                         .map_err(Error::Pool)?;
        self.projection.access::<G, _>(|index| {
                           *index.get_mut(wire).expect("exclusive unpublished child projection \
                                                        is retained until native publication") =
                               native.id();
                       });
        Ok(Publication::new(wire, native))
    }

    pub fn get(&self, wire: u32) -> Option<&T>
    {
        let native = self.projection.access::<G, _>(|index| index.get(wire).copied())?;
        let entry = self.values.get(native)?;
        (entry.wire == wire).then_some(&entry.value)
    }

    pub fn get_mut(&mut self, wire: u32) -> Option<&mut T>
    {
        let native = self.projection.access::<G, _>(|index| index.get(wire).copied())?;
        let entry = self.values.get_mut(native)?;
        (entry.wire == wire).then_some(&mut entry.value)
    }

    pub fn get_with_owner(&self, wire: u32) -> Option<(DomainId, &T)>
    {
        let native = self.projection.access::<G, _>(|index| index.get(wire).copied())?;
        let (owner, entry) = self.values.get_with_owner(native)?;
        (entry.wire == wire).then_some((owner, &entry.value))
    }

    pub fn iter(&self) -> impl Iterator<Item = (u32, &T)>
    {
        self.values.iter().map(|(_, entry)| (entry.wire, &entry.value))
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut T>
    {
        self.values.values_mut().map(|entry| &mut entry.value)
    }

    pub fn completion_key(&self, wire: u32) -> Option<CompletionKey>
    {
        let native = self.projection.access::<G, _>(|index| index.get(wire).copied())?;
        Some(CompletionKey { native: self.values.completion_key(native)?,
                             wire })
    }

    pub fn completing(&self, key: &CompletionKey) -> Option<&T>
    {
        let entry = self.values.completing(&key.native)?;
        (entry.wire == key.wire).then_some(&entry.value)
    }

    pub fn remove(&mut self, wire: u32) -> Option<Removed<T, G>>
    {
        let native = self.projection.access::<G, _>(|index| index.get(wire).copied())?;
        let record = self.values.remove(native)?;
        Some(Removed { wire,
                       record: Some(record) })
    }

    /// Inspect only through the unique invalidation capability. This does not
    /// revive an ordinary ID or expose mutable/extractable payload storage.
    pub fn releasing(&self, ticket: &ReleasedResource) -> Option<&T>
    {
        self.values.releasing(ticket).map(|entry| &entry.value)
    }

    pub fn release(&mut self, ticket: ReleasedResource) -> Result<Removed<T, G>, CompletionError>
    {
        let record = self.values.release(ticket)?;
        let wire = record.wire;
        Ok(Removed { wire,
                     record: Some(record) })
    }

    pub fn release_storage(&mut self, ticket: ReleasedResource) -> Result<(), CompletionError>
    {
        self.values.release_storage(ticket)
    }
}

/// Linear cleanup guard: payload and projection slot stay retained until its
/// destructor runs. An invalidated ordinary ID cannot access this payload.
/// Like the native store's guard, this deliberately permits no owned extraction.
pub struct Removed<T, G: InvocationGate>
{
    wire:   u32,
    record: Option<store::Removed<Entry<T>, G>>
}
impl<T, G: InvocationGate> Removed<T, G>
{
    pub fn wire(&self) -> u32
    {
        self.wire
    }
}
impl<T, G: InvocationGate> Deref for Removed<T, G>
{
    type Target = T;

    fn deref(&self) -> &T
    {
        &self.record.as_ref().expect("live projected removal guard").value
    }
}
impl<T, G: InvocationGate> Drop for Removed<T, G>
{
    fn drop(&mut self)
    {
        drop(self.record.take());
    }
}
