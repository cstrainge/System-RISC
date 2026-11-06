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
                // Inspect through the exact original ticket without creating
                // a temporary Root/Space reader which could wake its own Busy
                // retirement on every owner-walk turn.
                let observed = interrupts::with_masked(|| {
                    let store = manager.lock();
                    let root = store.releasing(&ticket).ok_or(Error::Authority)?;
                    root.closing.store(true, Ordering::Release);
                    if let Some(cause) = *root.retirement_failure.lock()
                    {
                        return Err(Error::Retirement(cause));
                    }
                    if !root.retired.load(Ordering::Acquire)
                    {
                        let notify = !root.retirement_requested.swap(true, Ordering::AcqRel);
                        return Ok((false, notify));
                    }
                    if root.retirement_claimed.load(Ordering::Acquire)
                    {
                        return Err(Error::Pending);
                    }
                    if root.registered.load(Ordering::Acquire) || root.backing.lock().is_some()
                    {
                        return Err(Error::Authority);
                    }
                    root.pin_waiters.swap(true, Ordering::AcqRel);
                    let _registry = ROOTS.lock();
                    let count = Arc::strong_count(root);
                    if count != 1
                    {
                        return Err(Error::Readers(count.saturating_sub(1)));
                    }
                    Ok((true, false))
                });
                let (ready, notify) = match observed
                {
                    Ok(observed) => observed,
                    Err(cause) => return Err(Failure::process_space(ticket, cause))
                };
                if notify
                {
                    retirement_changed();
                }
                if !ready
                {
                    let cause = if interrupts::with_masked(|| {
                        crate::machine::scheduler::has_threads(ticket.key().owner().incarnation())
                    })
                    {
                        Error::Threads
                    }
                    else
                    {
                        Error::Pending
                    };
                    return Err(Failure::process_space(ticket, cause));
                }
                // Every actual table/Node/attached branch and original physical
                // charge has already been destroyed after real ACK in primary
                // maintenance. Only canonical metadata/completion remains here.
                let old = interrupts::with_masked(|| {
                    manager.lock().reap_publication_capacity(ticket.key().owner())
                });
                drop(old);
                let removed =
                    interrupts::with_masked(|| manager.lock().release_with_backing(ticket));
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
