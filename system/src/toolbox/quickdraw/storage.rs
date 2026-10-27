//! Fallible backing admission shared by owned graphics snapshots and codecs.
#![forbid(unsafe_code)]
use alloc::{collections::TryReserveError, vec::Vec};
pub(super) fn vector<T>(capacity: usize) -> Result<Vec<T>, TryReserveError>
{
    let mut values = Vec::new();
    values.try_reserve_exact(capacity)?;
    Ok(values)
}
pub(super) fn copy<T: Copy>(source: &[T]) -> Result<Vec<T>, TryReserveError>
{
    let mut values = vector(source.len())?;
    values.extend_from_slice(source);
    Ok(values)
}
