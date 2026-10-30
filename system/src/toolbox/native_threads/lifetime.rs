//! Canonical TLS release retains the supplied ticket until actual pin/THRD
//! quiescence. It never invokes user callbacks during a resident owner walk.
use mac_object_table::{domain::ReleasedResource,
                       registry::{Release, Source, Specification}};

use super::*;
use crate::toolbox::handles::{CapacityPolicy, KindCode, cleanup::Failure};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CleanupCause
{
    Unavailable,
    Contract,
    Threads,
    Readers
    {
        count: usize
    },
    Preparing
    {
        count: usize
    }
}
impl fmt::Display for CleanupCause
{
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result
    {
        match self
        {
            Self::Contract =>
            {
                out.write_str("TLS cleanup registration mismatch; original ticket retained")
            }
            Self::Unavailable =>
            {
                out.write_str("TLS manager unavailable; original cleanup ticket retained")
            }
            Self::Threads =>
            {
                out.write_str("TLS root awaits actual Process thread/claim/slot quiescence")
            }
            Self::Readers { count } =>
            {
                write!(out, "TLS original resource retains {count} reader/destructor pins")
            }
            Self::Preparing { count } =>
            {
                write!(out, "TLS backing retains {count} genuine prepared publications")
            }
        }
    }
}
#[derive(Clone, Copy)]
pub(crate) enum Kind
{
    Root,
    RootStorage,
    Key,
    KeyStorage
}
impl Kind
{
    pub(crate) const ALL: [Self; 4] = [Self::Root, Self::RootStorage, Self::Key, Self::KeyStorage];

    pub(crate) fn specification(self) -> Specification<'static>
    {
        let (code, label, capacity) = match self
        {
            Self::Root =>
            {
                (*b"TLRT",
                 "Process TLS roots",
                 CapacityPolicy::HardBound { slots:  1,
                                             reason: "One canonical native TLS root per Process" })
            }
            Self::RootStorage =>
            {
                (*b"TLST",
                 "Process TLS root backing",
                 CapacityPolicy::HardBound { slots:  1,
                                             reason: "One TLS root value array per Process" })
            }
            Self::Key =>
            {
                (*b"TLSK",
                 "Native TLS keys",
                 CapacityPolicy::HardBound { slots:  crate::machine::memory::TLS_KEY_CAPACITY
                                                     as usize,
                                             reason: "Every actual thread prepays the \
                                                      platform-selected TLS key positions" })
            }
            Self::KeyStorage =>
            {
                (*b"TLSS",
                 "Native TLS key backing",
                 CapacityPolicy::HardBound { slots:  1,
                                             reason: "One TLS key value array per Process" })
            }
        };
        Specification { kind: KindCode(code),
                        label,
                        provider: "System native Thread Manager",
                        source: Source::Resident,
                        capacity }
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
            return Err(Failure::native_tls(ticket, CleanupCause::Contract));
        }
        let Some(manager) = MANAGER.get()
        else
        {
            return Err(Failure::native_tls(ticket, CleanupCause::Unavailable));
        };
        match self
        {
            Self::RootStorage =>
            {
                let result = interrupts::with_masked(|| {
                    let mut manager = manager.lock();
                    let count = manager.roots.publication_preparations(ticket.key().owner());
                    if count != 0
                    {
                        return Err(Failure::native_tls(ticket, CleanupCause::Preparing { count }));
                    }
                    manager.roots.release_storage_with_backing(ticket).map_err(Failure::from)
                })?;
                drop(result);
                Ok(())
            }
            Self::KeyStorage =>
            {
                let result = interrupts::with_masked(|| {
                    let mut manager = manager.lock();
                    let count = manager.keys.publication_preparations(ticket.key().owner());
                    if count != 0
                    {
                        return Err(Failure::native_tls(ticket, CleanupCause::Preparing { count }));
                    }
                    manager.keys.release_storage_with_backing(ticket).map_err(Failure::from)
                })?;
                drop(result);
                Ok(())
            }
            Self::Key =>
            {
                let actual =
                    interrupts::with_masked(|| manager.lock().keys.releasing(&ticket).cloned());
                let Some(actual) = actual
                else
                {
                    return Err(Failure::native_tls(ticket, CleanupCause::Contract));
                };
                let count = Arc::strong_count(&actual);
                if count != 2
                {
                    return Err(Failure::native_tls(ticket,CleanupCause::Readers { count: count.saturating_sub(2) }));
                }
                let removed =
                    interrupts::with_masked(|| manager.lock().keys.release_with_backing(ticket));
                drop(actual);
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
            Self::Root =>
            {
                let actual =
                    interrupts::with_masked(|| manager.lock().roots.releasing(&ticket).cloned());
                let Some(actual) = actual
                else
                {
                    return Err(Failure::native_tls(ticket, CleanupCause::Contract));
                };
                actual.closing.store(true, Ordering::Release);
                if interrupts::with_masked(|| {
                    crate::machine::scheduler::has_threads(actual.owner.owner().incarnation())
                })
                {
                    return Err(Failure::native_tls(ticket, CleanupCause::Threads));
                }
                let count = Arc::strong_count(&actual);
                if count != 2
                {
                    return Err(Failure::native_tls(ticket,CleanupCause::Readers { count: count.saturating_sub(2) }));
                }
                let retired = interrupts::with_masked(|| {
                    let mut manager = manager.lock();
                    (manager.keys.reap_publication_capacity(actual.owner.owner()),
                     manager.roots.reap_publication_capacity(actual.owner.owner()))
                });
                drop(retired);
                let removed =
                    interrupts::with_masked(|| manager.lock().roots.release_with_backing(ticket));
                drop(actual);
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
