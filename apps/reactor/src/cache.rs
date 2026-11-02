//! Sparse rows visited in one real membership/owner/ledger generation. Cache
//! capacity survives stale retries; observing our own heap must not continually
//! allocate/free the same rows. Managed controls still own only visible text.
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use mac_toolbox::{Error, ToolboxResult,
                  inspection::{Domain, MembershipRevision, OwnerKind}};
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Key
{
    membership:  MembershipRevision,
    kind:        OwnerKind,
    incarnation: u64,
    ledger:      u64
}
impl Key
{
    pub(super) fn new(membership: MembershipRevision, domain: Domain) -> Self
    {
        Self { membership,
               kind: domain.kind,
               incarnation: domain.incarnation,
               ledger: domain.ledger_revision }
    }
}
pub(super) struct Rows<T: Copy>
{
    key:  Cell<Option<Key>>,
    rows: RefCell<Vec<(u64, T)>>
}
impl<T: Copy> Rows<T>
{
    pub(super) fn new() -> Self
    {
        Self { key:  Cell::new(None),
               rows: RefCell::new(Vec::new()) }
    }

    pub(super) fn clear(&self)
    {
        self.rows.borrow_mut().clear();
        self.key.set(None);
    }

    pub(super) fn get_or_read(&self, key: Key, index: u64, read: impl FnOnce() -> ToolboxResult<T>)
                              -> ToolboxResult<T>
    {
        if self.key.get() != Some(key)
        {
            self.clear();
            self.key.set(Some(key));
        }
        let cached = {
            let rows = self.rows.borrow();
            rows.binary_search_by_key(&index, |&(at, _)| at).ok().map(|at| rows[at].1)
        };
        if let Some(value) = cached
        {
            return Ok(value);
        }
        {
            // Preflight before querying a ledger row: real application heap
            // growth can change our own ledger and force an honest stale read.
            self.rows.borrow_mut().try_reserve(1).map_err(|_| Error::NoMemory)?;
        }
        // No cache borrow spans inspection/resource traps. Only native UI code
        // owns this !Sync cache; callbacks never receive its storage/guards.
        let value = read()?;
        if self.key.get() != Some(key)
        {
            return Err(Error::ScanStale);
        }
        let mut rows = self.rows.borrow_mut();
        match rows.binary_search_by_key(&index, |&(at, _)| at)
        {
            Ok(at) => Ok(rows[at].1),
            Err(at) =>
            {
                // Reentrant invalidation cannot silently trigger a new heap
                // allocation after the read or publish a mixed-generation row.
                if rows.len() == rows.capacity()
                {
                    return Err(Error::ScanStale);
                }
                rows.insert(at, (index, value));
                Ok(value)
            }
        }
    }
}
