//! Failure admission closes author births independently of the original ledger,
//! which Finder needs accepting until System-owned Activity handoff completes.
use super::*;
pub(crate) fn fail_owner(admission: &crate::toolbox::native_threads::RuntimeAdmission)
                         -> Result<(), Status>
{
    if admission.owner().owner().kind() != DomainKind::Process
    {
        return Err(mac_abi::ACCESS_DENIED);
    }
    let owner = admission.owner().owner().incarnation();
    let count = {
        let _interrupts = super::super::super::interrupts::mask();
        let scheduler = SCHEDULER.lock();
        admission.close();
        for (_, thread) in scheduler.iter().filter(|(_, thread)| {
                                               thread.core.binding.owner() == owner
                                               && !thread.core.context.is_module_worker()
                                           })
        {
            thread.update(|state| {
                      state.failure_pending = true;
                      if !state.service.quiescent()
                      {
                          state.waiting = None;
                          state.woken = state.woken.max(1);
                      }
                  });
        }
        scheduler.threads.len()
    };
    // Host callbacks release original job/cancellation guards outside SCHEDULER.
    // No particular completion or privileged-fault recovery is fabricated.
    for slot in 0..count
    {
        let id = {
            let _interrupts = super::super::super::interrupts::mask();
            let scheduler = SCHEDULER.lock();
            scheduler.thread(slot)
                     .filter(|thread| {
                         thread.core.binding.owner() == owner
                         && !thread.core.context.is_module_worker()
                     })
                     .map(|thread| thread.id)
        };
        if let Some(id) = id
        {
            crate::drivers::discard_thread_jobs(id);
        }
    }
    crate::toolbox::processes::termination::progress();
    Ok(())
}
pub(crate) fn failure_quiescent(owner: &OwnerRef) -> Result<bool, Status>
{
    if owner.owner().kind() != DomainKind::Process
    {
        return Err(mac_abi::ACCESS_DENIED);
    }
    let _interrupts = super::super::super::interrupts::mask();
    let scheduler = SCHEDULER.lock();
    for (_, thread) in scheduler.iter().filter(|(_, thread)| {
                                           thread.core.binding.owner()
                                           == owner.owner().incarnation()
                                           && !thread.core.context.is_module_worker()
                                       })
    {
        let state = thread.state.get();
        if let Some(cause) = state.service.failure()
        {
            return Err(cause);
        }
        if state.claimed_by.is_some()
           || !state.service.quiescent()
           || (!state.failure_parked && state.live && !state.retired)
        {
            return Ok(false);
        }
    }
    Ok(true)
}
