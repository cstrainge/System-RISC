
use std::ops::{ Deref, DerefMut };

use crate::language::data::types::TypeId;


// Constructor annotations travel with a collection, independently of its owner binding.
#[derive(Clone, Debug)]
pub struct Collection<T>
{
    pub values: T,
    pub type_id: Option<TypeId>,
}

impl<T> From<T> for Collection<T>
{
    fn from(values: T) -> Self
    {
        Self { values, type_id: None }
    }
}

impl<T: PartialEq> PartialEq for Collection<T>
{
    fn eq(&self, other: &Self) -> bool
    {
        self.values == other.values
    }
}

impl<T> Deref for Collection<T>
{
    type Target = T;
    fn deref(&self) -> &T
    {
        &self.values
    }
}

impl<T> DerefMut for Collection<T>
{
    fn deref_mut(&mut self) -> &mut T
    {
        &mut self.values
    }
}

impl<T: IntoIterator> IntoIterator for Collection<T>
{
    type Item = T::Item;
    type IntoIter = T::IntoIter;
    fn into_iter(self) -> Self::IntoIter
    {
        self.values.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a mut Collection<T>
where &'a mut T: IntoIterator
{
    type Item = <&'a mut T as IntoIterator>::Item;
    type IntoIter = <&'a mut T as IntoIterator>::IntoIter;
    fn into_iter(self) -> Self::IntoIter
    {
        (&mut self.values).into_iter()
    }
}
