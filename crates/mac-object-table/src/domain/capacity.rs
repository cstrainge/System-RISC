//! Detached storage for the existing owner directory. Empty capacity grants no
//! identity, owner permission or reservation against another publisher.
#![forbid(unsafe_code)]
use alloc::vec::Vec;
use core::{alloc::Layout, fmt, marker::PhantomData};

use super::{Directory, DirectoryError, DomainEntry, DomainId, InvocationGate};
use crate::shared::OwnerRef;

/// A refused publication returns its authentic owner and payload untouched.
/// Dispose or retry them after leaving the directory's manager guard.
#[must_use = "retain or dispose the original owner and payload outside manager guards"]
pub struct DirectoryInsertFailure<T, G: InvocationGate>
{
    pub cause:  DirectoryError,
    pub ledger: OwnerRef<G>,
    pub value:  T
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryCapacityError
{
    Geometry
    {
        entries: usize
    },
    Storage
    {
        entries: usize
    },
    Changed
    {
        required: usize, prepared: usize
    }
}
impl fmt::Display for DirectoryCapacityError
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Geometry { entries } =>
            {
                write!(out,
                       "domain directory capacity geometry overflows for {entries} entries; no \
                        owner published")
            }
            Self::Storage { entries } =>
            {
                write!(out,
                       "not enough System storage to prepare domain directory capacity for \
                        {entries} entries; original directory retained")
            }
            Self::Changed { required,
                            prepared } =>
            {
                write!(out,
                       "domain directory now requires {required} entries but detached storage \
                        holds {prepared}; prepare new capacity and retry")
            }
        }
    }
}
impl core::error::Error for DirectoryCapacityError
{
}

/// Allocation observations only. The real directory revalidates capacity when
/// installing; this value cannot publish or retain a ledger on its own.
pub struct DirectoryCapacityPlan<T, G: InvocationGate>
{
    capacity: usize,
    _entry:   PhantomData<fn() -> DomainEntry<T, G>>
}
/// New or displaced empty storage. Dispose after leaving directory/manager
/// guards; no caller's live DomainEntry is destroyed during installation.
#[must_use = "dispose prepared and obsolete directory capacity outside manager guards"]
pub struct PreparedDirectoryCapacity<T, G: InvocationGate>
{
    entries: Vec<DomainEntry<T, G>>
}
impl<T, G: InvocationGate> DirectoryCapacityPlan<T, G>
{
    pub fn prepare(self) -> Result<PreparedDirectoryCapacity<T, G>, DirectoryCapacityError>
    {
        Layout::array::<DomainEntry<T, G>>(self.capacity)
            .map_err(|_| DirectoryCapacityError::Geometry { entries: self.capacity })?;
        let mut entries = Vec::new();
        G::with_system_allocation(|| entries.try_reserve_exact(self.capacity))
            .map_err(|_| DirectoryCapacityError::Storage { entries: self.capacity })?;
        Ok(PreparedDirectoryCapacity { entries })
    }
}
impl<T, G: InvocationGate> Directory<T, G>
{
    /// Publish into already admitted storage. This never allocates or destroys
    /// the rejected entry. Capacity is checked again in the publication turn.
    pub fn insert_prepared(&mut self, ledger: OwnerRef<G>, value: T)
                           -> Result<DomainId, DirectoryInsertFailure<T, G>>
    {
        let owner = ledger.owner();
        let at =
            match self.entries.binary_search_by_key(&owner.incarnation(), |entry| {
                                  entry.owner().incarnation()
                              })
            {
                Ok(_) =>
                {
                    return Err(DirectoryInsertFailure { cause: DirectoryError::Duplicate { owner },
                                                        ledger,
                                                        value });
                }
                Err(at) => at
            };
        if self.entries.len() == self.entries.capacity()
        {
            return Err(DirectoryInsertFailure { cause: DirectoryError::Storage { owner },
                                                ledger,
                                                value });
        }
        Ok(self.insert_at(at, ledger, value))
    }

    /// Observe space for one later birth without allocating under the manager.
    pub fn plan_insert_capacity(&self)
                                -> Result<DirectoryCapacityPlan<T, G>, DirectoryCapacityError>
    {
        let required =
            self.entries
                .len()
                .checked_add(1)
                .ok_or(DirectoryCapacityError::Geometry { entries: self.entries.len() })?;
        Ok(DirectoryCapacityPlan { capacity: if self.entries.capacity() < required
                                   {
                                       required
                                   }
                                   else
                                   {
                                       0
                                   },
                                   _entry:   PhantomData })
    }

    /// Install enough empty storage for the next insert in this same manager
    /// turn. A later publisher must revalidate again. Capacity changes do not
    /// change directory contents/revision or issue an identity.
    pub fn install_insert_capacity(&mut self, prepared: &mut PreparedDirectoryCapacity<T, G>)
                                   -> Result<(), DirectoryCapacityError>
    {
        let required =
            self.entries
                .len()
                .checked_add(1)
                .ok_or(DirectoryCapacityError::Geometry { entries: self.entries.len() })?;
        if self.entries.capacity() >= required
        {
            return Ok(());
        }
        if prepared.entries.capacity() < required
        {
            return Err(DirectoryCapacityError::Changed { required,
                                                         prepared: prepared.entries.capacity() });
        }
        // Checked capacity covers every move and the pending insert. Moving
        // genuine entries preserves their ledgers/payloads and sorted order.
        prepared.entries.extend(self.entries.drain(..));
        core::mem::swap(&mut self.entries, &mut prepared.entries);
        Ok(())
    }
}
