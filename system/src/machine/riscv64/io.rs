//! Synchronous fixed-owner firmware-I/O identity handoff. Resident work already
//! uses its admitted hart's System stack. Foreign CPUs retain their argument
//! and receive WrongOwner before any opaque firmware callback can run.
use crate::state::AccessError;

pub fn run<F: FnOnce() -> R, R>(function: F) -> Result<R, AccessError>
{
    run_owned((), |()| function()).map_err(|(error, ())| error)
}
pub fn run_owned<A, F: FnOnce(A) -> R, R>(argument: A, function: F) -> Result<R, (AccessError, A)>
{
    super::interrupts::with_masked(|| {
        let owner = super::paging::firmware_owner_hart();
        if owner.is_none() || super::traps::execution::hart_id() != owner
        {
            return Err((AccessError::WrongOwner, argument));
        }
        if super::scheduler::current_context().is_bootstrap()
        {
            return Ok(function(argument));
        }
        struct RestoreIo(bool);
        impl Drop for RestoreIo
        {
            fn drop(&mut self)
            {
                super::scheduler::io_leave(self.0);
            }
        }
        let resources = crate::kernel::current_resource_selection();
        let subject = crate::context::io_subject();
        let allocation = crate::context::selected_allocation();
        let recovery = crate::context::recovery_allocation();
        let activity = crate::context::recovery_activity();
        let previous = match super::scheduler::io_enter()
        {
            Ok(previous) => previous,
            Err(error) => return Err((error, argument))
        };
        let _restore = RestoreIo(previous);
        Ok(crate::context::with_bootstrap_recovery_allocation(recovery, || {
            crate::context::with_bootstrap_recovery_activity(activity, || {
                crate::context::with_bootstrap_allocation(allocation, || {
                    crate::context::with_bootstrap_io_subject(subject, || {
                        crate::context::with_bootstrap_resources(Some(resources), || {
                            function(argument)
                        })
                    })
                })
            })
        }))
    })
}
