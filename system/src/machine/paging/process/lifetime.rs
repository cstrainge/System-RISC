//! Canonical SPCE disposal retains the exact ticket until real execution and
//! translation quiescence. No table/backing destructor runs under Store guards.
use mac_object_table::{domain::ReleasedResource,
                       registry::{Release, Source, Specification}};

use super::*;
use crate::toolbox::handles::{CapacityPolicy, KindCode, cleanup::Failure};
#[derive(Clone, Copy)]
pub(crate) enum Kind
{
    Root,
    Storage
}
impl Kind
{
    pub(crate) const ALL: [Self; 2] = [Self::Root, Self::Storage];

    pub(crate) fn specification(self) -> Specification<'static>
    {
        Specification { kind:     KindCode(match self
                                           {
                                               Self::Root => *b"SPCE",
                                               Self::Storage => *b"SPST"
                                           }),
                        label:    match self
                        {
                            Self::Root => "Process supervisor address spaces",
                            Self::Storage => "Process space backing"
                        },
                        provider: "System process paging",
                        source:   Source::Resident,
                        capacity: CapacityPolicy::HardBound { slots:  1,
                                                              reason: "One canonical execution \
                                                                       address space per Process" } }
    }
}
impl Release<dyn crate::toolbox::handles::cleanup::context::Context, Failure> for Kind
{
    fn release(&self, _: &mut (dyn crate::toolbox::handles::cleanup::context::Context + 'static),
               ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        if ticket.key().kind() != self.specification().kind
        {
            return Err(Failure::process_space(ticket, Error::Authority));
        }
        let Some(manager) = MANAGER.get()
        else
        {
            return Err(Failure::process_space(ticket, Error::Missing));
        };
        match self
        {
            Self::Storage =>
            {
                let removed = interrupts::with_masked(|| {
                    let mut store = manager.lock();
                    let count = store.publication_preparations(ticket.key().owner());
                    if count != 0
                    {
                        return Err(Failure::process_space(ticket, Error::Readers(count)));
                    }
                    store.release_storage_with_backing(ticket).map_err(Failure::from)
                })?;
                drop(removed);
                Ok(())
            }
            Self::Root =>
            {
                let root = interrupts::with_masked(|| manager.lock().releasing(&ticket).cloned());
                let Some(root) = root
                else
                {
                    return Err(Failure::process_space(ticket, Error::Authority));
                };
                root.closing.store(true, Ordering::Release);
                if interrupts::with_masked(|| {
                    crate::machine::scheduler::has_threads(root.owner.owner().incarnation())
                })
                {
                    return Err(Failure::process_space(ticket, Error::Threads));
                }
                let expected = if root.registered.load(Ordering::Acquire) { 3 } else { 2 };
                WAITING_PINS.store(true, Ordering::Release);
                let count = interrupts::with_masked(|| {
                    let _registry = ROOTS.lock();
                    Arc::strong_count(&root)
                });
                if count != expected
                {
                    return Err(Failure::process_space(ticket,Error::Readers(count.saturating_sub(expected))));
                }
                if let Err(cause) = retire(&root)
                {
                    return Err(Failure::process_space(ticket, cause));
                }
                let old = interrupts::with_masked(|| {
                    manager.lock().reap_publication_capacity(root.owner.owner())
                });
                drop(old);
                let removed =
                    interrupts::with_masked(|| manager.lock().release_with_backing(ticket));
                drop(root);
                match removed
                {
                    Ok(removed) =>
                    {
                        removed.dispose();
                        Ok(())
                    }
                    Err(cause) => Err(cause.into())
                }
            }
        }
    }
}
