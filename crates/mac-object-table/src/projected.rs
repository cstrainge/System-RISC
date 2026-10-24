//! Existing 32-bit treaty namespaces projected onto registered owner storage.
//! The projection is an index, never a second payload or ownership authority.
use core::{fmt, ops::Deref};

use crate::{Kind, Table,
            domain::{CompletionError, PoolError, ReleasedResource},
            registry::KindLease,
            shared::{InvocationGate, OwnerRef},
            store};

#[derive(Debug)]
pub enum Error
{
    UnscopedProjection,
    ProjectionExhausted,
    Pool(PoolError)
}
impl fmt::Display for Error
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::UnscopedProjection =>
            {
                f.write_str("object projection requires boot-wide generation history; this \
                             namespace has not migrated")
            }
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
pub struct Store<T, G: InvocationGate>
{
    projection: Table<u64>,
    values:     store::Store<Entry<T>, G>
}
impl<T, G: InvocationGate> Store<T, G>
{
    pub fn new(wire: Kind, payload: KindLease<G>, backing: KindLease<G>) -> Result<Self, Error>
    {
        // Refuse a namespace whose table could recreate a foreign/stale wire ID.
        // Adding a customer requires its shared generation history first.
        if wire.shared_generation(0).is_none()
        {
            return Err(Error::UnscopedProjection);
        }
        Ok(Self { projection: Table::new(wire),
                  values:     store::Store::owned(payload, backing).map_err(Error::Pool)? })
    }

    pub fn insert(&mut self, owner: &OwnerRef<G>, value: T) -> Result<u32, Error>
    {
        if !owner.accepting()
        {
            return Err(Error::Pool(PoolError::Stopping { owner: owner.owner() }));
        }
        // No partially admitted object can be reached through an ordinary ID.
        // Generation history is consumed even when later publication refuses.
        let wire = self.projection.insert(0).map_err(|_| Error::ProjectionExhausted)?;
        let native = match self.values.insert(owner,
                                              Entry { wire,
                                                      value })
        {
            Ok(id) => id,
            Err(error) =>
            {
                self.projection.remove(wire);
                return Err(Error::Pool(error));
            }
        };
        *self.projection.get_mut(wire).expect("exclusive provisional projection") = native;
        Ok(wire)
    }

    pub fn get(&self, wire: u32) -> Option<&T>
    {
        let entry = self.values.get(*self.projection.get(wire)?)?;
        (entry.wire == wire).then_some(&entry.value)
    }

    pub fn get_mut(&mut self, wire: u32) -> Option<&mut T>
    {
        let entry = self.values.get_mut(*self.projection.get(wire)?)?;
        (entry.wire == wire).then_some(&mut entry.value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (u32, &T)>
    {
        self.values.iter().map(|(_, entry)| (entry.wire, &entry.value))
    }

    pub fn remove(&mut self, wire: u32) -> Option<Removed<T, G>>
    {
        let record = self.values.remove(*self.projection.get(wire)?)?;
        self.projection.remove(record.wire);
        Some(Removed(record))
    }

    pub fn release(&mut self, ticket: ReleasedResource) -> Result<Removed<T, G>, CompletionError>
    {
        let record = self.values.release(ticket)?;
        self.projection.remove(record.wire);
        Ok(Removed(record))
    }

    pub fn release_storage(&mut self, ticket: ReleasedResource) -> Result<(), CompletionError>
    {
        self.values.release_storage(ticket)
    }
}

/// Linear cleanup guard: the payload stays retained until its destructor runs.
/// Like the native store's guard, this deliberately permits no owned extraction.
pub struct Removed<T, G: InvocationGate>(store::Removed<Entry<T>, G>);
impl<T, G: InvocationGate> Removed<T, G>
{
    pub fn wire(&self) -> u32
    {
        self.0.wire
    }
}
impl<T, G: InvocationGate> Deref for Removed<T, G>
{
    type Target = T;

    fn deref(&self) -> &T
    {
        &self.0.value
    }
}
