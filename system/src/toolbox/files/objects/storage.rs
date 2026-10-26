//! File payloads share one lifetime algorithm. These adapters select only the
//! shared facility's native identity or genuine classic treaty projection.
#![forbid(unsafe_code)]
use core::ops::Deref;

use mac_object_table::{domain::{CompletionError, ReleasedResource},
                       projected, store};

use super::{Payload, Status};
use crate::toolbox::handles::{InvocationGate, KindLease, OwnerRef};

pub(in crate::toolbox::files) trait Backend<T: Payload>: Sized
{
    type Id: Copy + Eq + Ord + Default + core::fmt::Display + Into<u64>;
    type Key: Copy;
    type Guard: Deref<Target = T>;
    fn new(kinds: (KindLease, KindLease)) -> Result<Self, Status>;
    fn insert(&mut self, owner: &OwnerRef, value: T) -> Result<(Self::Id, Self::Key), Status>;
    fn insert_scoped(&mut self, scope: &super::super::subject::Lease, value: T)
                     -> Result<(Self::Id, Self::Key), Status>;
    fn insert_root(&mut self, reservation: super::super::subject::Root,
                   make: impl FnOnce(Self::Id) -> T)
                   -> Result<(Self::Id, Self::Key), Status>;
    fn get(&self, id: Self::Id) -> Option<&T>;
    fn get_mut(&mut self, id: Self::Id) -> Option<&mut T>;
    fn completing(&self, key: &Self::Key) -> Option<&T>;
    fn remove(&mut self, id: Self::Id) -> Option<Self::Guard>;
    fn releasing(&self, ticket: &ReleasedResource) -> Option<&T>;
    fn release(&mut self, ticket: ReleasedResource) -> Result<Self::Guard, CompletionError>;
    fn release_storage(&mut self, ticket: ReleasedResource) -> Result<(), CompletionError>;
    fn id(guard: &Self::Guard) -> Self::Id;
}
pub(in crate::toolbox::files) type Id<T> = <<T as Payload>::Storage as Backend<T>>::Id;
pub(in crate::toolbox::files) type Key<T> = <<T as Payload>::Storage as Backend<T>>::Key;
pub(in crate::toolbox::files) type Guard<T> = <<T as Payload>::Storage as Backend<T>>::Guard;
fn status(error: projected::Error) -> Status
{
    #[cfg(target_arch = "riscv64")]
    serial!("FILE_OBJECT_ADMISSION_REFUSED: {error}");
    crate::toolbox::handles::projection_status(error)
}
pub(in crate::toolbox::files) struct Native<T>(store::Store<T, InvocationGate>);
pub(in crate::toolbox::files) struct Treaty<T>(projected::Store<T, InvocationGate>);
impl<T: Payload> Backend<T> for Native<T>
{
    type Guard = store::Removed<T, InvocationGate>;
    type Id = u64;
    type Key = store::CompletionKey;

    fn new(kinds: (KindLease, KindLease)) -> Result<Self, Status>
    {
        store::Store::owned(kinds.0, kinds.1).map(Self)
                                             .map_err(|error| status(projected::Error::Pool(error)))
    }

    fn insert(&mut self, owner: &OwnerRef, value: T) -> Result<(u64, Self::Key), Status>
    {
        self.0
            .insert_with_receipt(owner, |_| value)
            .map(store::Publication::into_parts)
            .map_err(|error| status(projected::Error::Pool(error)))
    }

    fn insert_scoped(&mut self, scope: &super::super::subject::Lease, value: T)
                     -> Result<(u64, Self::Key), Status>
    {
        self.0
            .insert_in_scope_with_receipt(scope, |_| value)
            .map(store::Publication::into_parts)
            .map_err(|error| status(projected::Error::Pool(error)))
    }

    fn insert_root(&mut self, reservation: super::super::subject::Root,
                   make: impl FnOnce(u64) -> T)
                   -> Result<(u64, Self::Key), Status>
    {
        self.0
            .insert_root_with_receipt(reservation, make)
            .map(store::Publication::into_parts)
            .map_err(|error| status(projected::Error::Pool(error)))
    }

    fn get(&self, id: u64) -> Option<&T>
    {
        self.0.get(id)
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut T>
    {
        self.0.get_mut(id)
    }

    fn completing(&self, key: &Self::Key) -> Option<&T>
    {
        self.0.completing(key)
    }

    fn remove(&mut self, id: u64) -> Option<Self::Guard>
    {
        self.0.remove(id)
    }

    fn releasing(&self, ticket: &ReleasedResource) -> Option<&T>
    {
        self.0.releasing(ticket)
    }

    fn release(&mut self, ticket: ReleasedResource) -> Result<Self::Guard, CompletionError>
    {
        self.0.release(ticket)
    }

    fn release_storage(&mut self, ticket: ReleasedResource) -> Result<(), CompletionError>
    {
        self.0.release_storage(ticket)
    }

    fn id(guard: &Self::Guard) -> u64
    {
        guard.id()
    }
}
impl<T: Payload> Backend<T> for Treaty<T>
{
    type Guard = projected::Removed<T, InvocationGate>;
    type Id = u32;
    type Key = projected::CompletionKey;

    fn new(kinds: (KindLease, KindLease)) -> Result<Self, Status>
    {
        projected::Store::new(T::WIRE.ok_or(mac_abi::BAD_ABI)?, kinds.0, kinds.1).map(Self)
                                                                                 .map_err(status)
    }

    fn insert(&mut self, owner: &OwnerRef, value: T) -> Result<(u32, Self::Key), Status>
    {
        self.0
            .insert_with_receipt(owner, value)
            .map(projected::Publication::into_parts)
            .map_err(status)
    }

    fn insert_scoped(&mut self, scope: &super::super::subject::Lease, value: T)
                     -> Result<(u32, Self::Key), Status>
    {
        self.0
            .insert_in_scope_with_receipt(scope, value)
            .map(projected::Publication::into_parts)
            .map_err(status)
    }

    fn insert_root(&mut self, reservation: super::super::subject::Root,
                   make: impl FnOnce(u32) -> T)
                   -> Result<(u32, Self::Key), Status>
    {
        self.0
            .insert_root_with_receipt(reservation, make)
            .map(projected::Publication::into_parts)
            .map_err(status)
    }

    fn get(&self, id: u32) -> Option<&T>
    {
        self.0.get(id)
    }

    fn get_mut(&mut self, id: u32) -> Option<&mut T>
    {
        self.0.get_mut(id)
    }

    fn completing(&self, key: &Self::Key) -> Option<&T>
    {
        self.0.completing(key)
    }

    fn remove(&mut self, id: u32) -> Option<Self::Guard>
    {
        self.0.remove(id)
    }

    fn releasing(&self, ticket: &ReleasedResource) -> Option<&T>
    {
        self.0.releasing(ticket)
    }

    fn release(&mut self, ticket: ReleasedResource) -> Result<Self::Guard, CompletionError>
    {
        self.0.release(ticket)
    }

    fn release_storage(&mut self, ticket: ReleasedResource) -> Result<(), CompletionError>
    {
        self.0.release_storage(ticket)
    }

    fn id(guard: &Self::Guard) -> u32
    {
        guard.wire()
    }
}
