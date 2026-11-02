//! Bounded clock query for the actual admitted module invocation. No registry,
//! allocation, worker-token lookup or foreign callback participates in this leaf.
use mac_abi::{ACCESS_DENIED, BAD_ABI, BAD_ARGUMENT, OK, Status};

/// # Safety
/// The native SDK lends one aligned, exclusive live u64 output until return.
/// A copied context number does not authorize this call without the installed
/// matching module invocation. The service lease preserves that invocation.
pub(super) unsafe extern "C" fn monotonic(module: u64, output: *mut u64) -> Status
{
    if crate::machine::traps::execution::policy::deny(
        crate::machine::traps::execution::policy::Operation::Service)
    {
        return ACCESS_DENIED;
    }
    // Denial precedes routing and all caller-pointer access, just like the
    // other native Host boundaries. No metadata or module lock is acquired here.
    let _service = match crate::machine::scheduler::service::host::acquire()
    {
        Ok(lease) => lease,
        Err(status) => return status
    };
    if let Err(status) = crate::context::authenticate_module(module)
    {
        return status;
    }
    if output.is_null() || !output.is_aligned()
    {
        return BAD_ARGUMENT;
    }
    match crate::machine::monitor::ticks_to_microseconds(crate::platform::ticks())
    {
        Ok(microseconds) =>
        {
            // SAFETY: only the authenticated SDK's checked exclusive output is
            // written, while the same synchronous invocation lease remains held.
            unsafe { output.write(microseconds) };
            OK
        }
        Err(_) => BAD_ABI
    }
}
