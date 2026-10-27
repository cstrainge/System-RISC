//! Immutable region backing: rectangles stay inline; complex clips share their
//! admitted arrays. Builders alone mutate data, before a Region is published.
#![forbid(unsafe_code)]
use alloc::{sync::Arc, vec::Vec};
use core::ops::{Deref, DerefMut};

use super::RegionError;
#[derive(Clone, Debug, PartialEq, Eq)]
enum Backing<T>
{
    Empty,
    One(T),
    Shared(Arc<Vec<T>>)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedSlice<T>(Backing<T>);
impl<T> Default for SharedSlice<T>
{
    fn default() -> Self
    {
        Self(Backing::Empty)
    }
}
impl<T> SharedSlice<T>
{
    pub(super) fn one(value: T) -> Self
    {
        Self(Backing::One(value))
    }
}
impl<T> Deref for SharedSlice<T>
{
    type Target = [T];

    fn deref(&self) -> &[T]
    {
        match &self.0
        {
            Backing::Empty => &[],
            Backing::One(value) => core::slice::from_ref(value),
            Backing::Shared(values) => values
        }
    }
}
impl<'a, T> IntoIterator for &'a SharedSlice<T>
{
    type IntoIter = core::slice::Iter<'a, T>;
    type Item = &'a T;

    fn into_iter(self) -> Self::IntoIter
    {
        self.iter()
    }
}
/// First value needs no allocation. Promotion and every subsequent growth are
/// fallible; freezing a complex array admits its shared owner exactly once.
pub(super) struct Values<T>
{
    first: Option<T>,
    rest:  Vec<T>
}
impl<T> Values<T>
{
    pub(super) fn new() -> Self
    {
        Self { first: None,
               rest:  Vec::new() }
    }

    pub(super) fn push(&mut self, value: T) -> Result<(), RegionError>
    {
        if self.rest.is_empty()
        {
            if self.first.is_none()
            {
                self.first = Some(value);
                return Ok(());
            }
            self.rest.try_reserve_exact(2).map_err(|_| RegionError::Allocation)?;
            if let Some(first) = self.first.take()
            {
                self.rest.push(first);
            }
        }
        self.rest.try_reserve(1).map_err(|_| RegionError::Allocation)?;
        self.rest.push(value);
        Ok(())
    }

    pub(super) fn finish(self) -> Result<SharedSlice<T>, RegionError>
    {
        if self.rest.is_empty()
        {
            Ok(self.first.map_or_else(SharedSlice::default, SharedSlice::one))
        }
        else
        {
            Arc::try_new(self.rest).map(|rows| SharedSlice(Backing::Shared(rows)))
                                   .map_err(|_| RegionError::Allocation)
        }
    }
}
impl<T> Deref for Values<T>
{
    type Target = [T];

    fn deref(&self) -> &[T]
    {
        if self.rest.is_empty() { self.first.as_slice() } else { &self.rest }
    }
}
impl<T> DerefMut for Values<T>
{
    fn deref_mut(&mut self) -> &mut [T]
    {
        if self.rest.is_empty() { self.first.as_mut_slice() } else { &mut self.rest }
    }
}
impl<'a, T> IntoIterator for &'a Values<T>
{
    type IntoIter = core::slice::Iter<'a, T>;
    type Item = &'a T;

    fn into_iter(self) -> Self::IntoIter
    {
        self.iter()
    }
}
