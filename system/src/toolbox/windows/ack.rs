//! Actual original ledger completion, distinct from payload/Removed Drop.
#![forbid(unsafe_code)]
use core::sync::atomic::{AtomicBool, Ordering};

use mac_object_table::domain::ResourceKey;

use super::*;

static WAITING: AtomicBool = AtomicBool::new(false);
/// Actual native completion-gate event only. Never call on an AccountUse drop
/// or an unchanged failed cleanup attempt. It conveys no ACK authority.
pub(crate) fn acknowledgement_changed()
{
    if WAITING.load(Ordering::Acquire) && WAITING.swap(false, Ordering::AcqRel)
    {
        crate::kernel::window_sources_changed();
    }
}
pub(super) struct Receipt
{
    owner:     OwnerRef,
    key:       Option<ResourceKey>,
    destroyed: bool,
    complete:  bool
}
impl Receipt
{
    pub(super) fn new(owner: OwnerRef) -> Self
    {
        Self { owner,
               key: None,
               destroyed: false,
               complete: false }
    }

    pub(super) fn owner(&self) -> &OwnerRef
    {
        &self.owner
    }

    pub(super) fn bind(&mut self, key: ResourceKey)
    {
        self.key = Some(key);
    }

    pub(super) fn destroyed(&mut self)
    {
        self.destroyed = true;
    }

    pub(super) fn refresh(&mut self) -> bool
    {
        if self.complete || !self.destroyed
        {
            return self.complete;
        }
        let Some(key) = self.key
        else
        {
            return false;
        };
        if key.owner() != self.owner.owner()
        {
            return false;
        }
        // Most payload drops have already ACKed. Do not arm a global wake for
        // that path. If still pending, arm and recheck so a racing real ACK is
        // either observed here or posts the availability edge.
        if !self.owner.pending(key) && !self.owner.contains(key)
        {
            self.complete = true;
            return true;
        }
        WAITING.store(true, Ordering::Release);
        self.complete = !self.owner.pending(key) && !self.owner.contains(key);
        self.complete
    }
}
