//! Detached storage for the existing owner directory. Empty capacity grants no
//! identity, owner permission or reservation against another publisher.
#![forbid(unsafe_code)]
use alloc::vec::Vec;
use core::{alloc::Layout, fmt, marker::PhantomData};

use super::{Directory, DomainEntry, InvocationGate};

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
