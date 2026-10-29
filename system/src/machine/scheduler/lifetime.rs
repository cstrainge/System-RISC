//! Thread registrations participate in the common invalidate-first walk. A
//! dispatch parks its unique ticket; only the scheduler's real reaper ACKs it.
#![forbid(unsafe_code)]
use mac_object_table::{domain::{PoolError, ReleasedResource},
                       registry::{CleanupFailure, Release, Source, Specification}};

use super::*;
use crate::toolbox::handles::{CapacityPolicy, KindCode, cleanup::Failure};

pub(crate) fn specification(storage: bool) -> Specification<'static>
{
    Specification { kind:     KindCode(if storage { *b"THST" } else { *b"THRD" }),
                    label:    if storage { "Thread backing" } else { "Threads" },
                    provider: "System scheduler",
                    source:   Source::Resident,
                    capacity: if storage
                    {
                        CapacityPolicy::HardBound { slots:  1,
                                                    reason: "One scheduler backing per owner \
                                                             domain" }
                    }
                    else
                    {
                        CapacityPolicy::GrowableUntilMemory
                    } }
}
pub(crate) fn pool_status(error: PoolError) -> mac_abi::Status
{
    serial!("THREAD_ADMISSION_REFUSED: {error}");
    crate::toolbox::handles::projection_status(mac_object_table::projected::Error::Pool(error))
}
pub(crate) struct ReleaseThreads(pub bool);
impl Release<dyn crate::toolbox::handles::cleanup::context::Context, Failure> for ReleaseThreads
{
    fn release(&self, _: &mut (dyn crate::toolbox::handles::cleanup::context::Context + 'static),
               ticket: ReleasedResource)
               -> Result<(), Failure>
    {
        let mut scheduler = SCHEDULER.lock();
        let Some(store) = scheduler.records.as_mut()
        else
        {
            return Err(Failure::context_unavailable(ticket, "registered scheduler storage"));
        };
        if self.0
        {
            return store.release_storage(ticket).map_err(Failure::from);
        }
        let Some(record) = store.releasing(&ticket)
        else
        {
            return Err(Failure::registration_mismatch(ticket));
        };
        let thread = match record
        {
            Record::Scheduled(thread) => thread,
            Record::SystemIdle(idle) =>
            {
                serial!("SYSTEM_IDLE_RELEASE_RETAINED thread={} hart={} reason=reset-lifetime",
                        idle.id,
                        idle.core.target.number());
                return Err(Failure::context_unavailable(ticket,
                                                        "reset-lifetime System idle remains \
                                                         owned until machine reset"));
            }
        };
        let mut pending = thread.core.release.lock();
        if let Some(previous) = pending.take()
        {
            *pending = Some(previous);
            return Err(Failure::registration_mismatch(ticket));
        }
        if thread.worker.is_some()
        {
            cancel_worker(thread, true);
        }
        else
        {
            retire(thread);
        }
        *pending = Some(ticket);
        Ok(())
    }
}
