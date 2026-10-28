//! The module metadata directory owns each genuine module ledger. Incarnations
//! come only from the shared facility; payload mutation cannot swap the ledger.
#![forbid(unsafe_code)]
use mac_object_table::domain::{Directory, DomainKind, IndexError, OwnerIndex};

use super::Error;
use crate::toolbox::handles::InvocationGate;

pub(super) struct Modules<T>
{
    entries: Directory<T, InvocationGate>
}
impl<T> Modules<T>
{
    pub(super) const fn new() -> Self
    {
        Self { entries: Directory::new() }
    }

    pub(super) fn insert(&mut self, value: T) -> Result<u64, Error>
    {
        let ledger = OwnerIndex::issue(DomainKind::Module).map_err(|error| {
                         match error
                         {
                             IndexError::Identity(error) => Error::Identity(error),
                             IndexError::AdmissionStorage { .. }
                             | IndexError::SharedStorage { .. } => Error::Exhausted,
                             IndexError::Pool(error) => Error::Owner(error)
                         }
                     })?;
        let id = self.entries.insert(ledger, value).map_err(Error::Domain)?.incarnation();
        #[cfg(target_arch = "riscv64")]
        crate::toolbox::inspection::membership_changed();
        Ok(id)
    }

    pub(super) fn directory(&self) -> &Directory<T, InvocationGate>
    {
        &self.entries
    }

    pub(super) fn get(&self, id: u64) -> Option<&T>
    {
        self.entries.get(id).map(|entry| entry.value())
    }

    pub(super) fn get_mut(&mut self, id: u64) -> Option<&mut T>
    {
        self.entries.get_mut(id)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (u64, &T)>
    {
        self.entries.iter().map(|entry| (entry.owner().incarnation(), entry.value()))
    }

    pub(super) fn stop(&mut self, id: u64) -> Result<(), Error>
    {
        let changed = self.entries.get(id).is_some_and(|entry| entry.ledger().accepting());
        self.entries.stop_admission(id).map_err(Error::Domain)?;
        #[cfg(target_arch = "riscv64")]
        if changed
        {
            crate::toolbox::inspection::membership_changed();
        }
        #[cfg(not(target_arch = "riscv64"))]
        let _ = changed;
        Ok(())
    }

    pub(super) fn remove(&mut self, id: u64) -> Result<T, Error>
    {
        let value = self.entries.remove(id).map_err(Error::Domain)?;
        #[cfg(target_arch = "riscv64")]
        crate::toolbox::inspection::membership_changed();
        Ok(value)
    }
}
