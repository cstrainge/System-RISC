//! The module metadata directory owns each genuine module ledger. Incarnations
//! come only from the shared facility; payload mutation cannot swap the ledger.
#![forbid(unsafe_code)]
use mac_object_table::domain::Directory;

use super::Error;
use crate::toolbox::handles::{InvocationGate, OwnerRef};

/// Uses the existing on-disk contract bound; it adds no new module limit.
/// The original label survives account drainage in the shared directory row.
pub(super) struct Name
{
    bytes:  [u8; mac_module::MAX_NAME_BYTES],
    length: usize
}
impl Name
{
    pub(super) fn new(text: &str) -> Result<Self, Error>
    {
        if text.is_empty() || text.len() > mac_module::MAX_NAME_BYTES
        {
            return Err(Error::Contract(mac_module::Error::Limit));
        }
        let mut name = Self { bytes:  [0; mac_module::MAX_NAME_BYTES],
                              length: text.len() };
        name.bytes[..text.len()].copy_from_slice(text.as_bytes());
        Ok(name)
    }

    pub(super) fn text(&self) -> &str
    {
        match core::str::from_utf8(&self.bytes[..self.length])
        {
            Ok(text) => text,
            Err(_) => "Invalid admitted module name UTF-8"
        }
    }
}

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

    pub(super) fn insert(&mut self, ledger: OwnerRef, value: T) -> Result<u64, Error>
    {
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
