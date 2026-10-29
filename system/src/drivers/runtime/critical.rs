//! Native module critical sections. Exclusion and nesting belong to the actual
//! OS thread; independently linked modules do not invent per-module TLS or an
//! interrupt mask. The author-facing SDK lends its state only in this callback.
use core::ffi::c_void;

use mac_abi::{BAD_ABI, OK, Status};
use mac_module::abi::AllocationOperation;

/// # Safety
/// Callback is a synchronous thunk in this module's pinned executable ranges.
/// Its opaque stack record remains exclusively borrowed until return; no Rust
/// trait layout, guard or reference may escape the callback boundary.
pub(super) unsafe extern "C" fn enter(module: u64, callback: AllocationOperation,
                                      state: *mut c_void)
                                      -> Status
{
    if crate::machine::traps::execution::policy::deny(
        crate::machine::traps::execution::policy::Operation::Critical)
    {
        return mac_abi::ACCESS_DENIED;
    }
    let operation = || -> Result<Status, Status> {
        crate::context::authenticate_module(module)?;
        super::invocation::callback_allowed(module, callback as usize)?;
        crate::context::with_critical_section(|| {
            // SAFETY: the genuine pinned module contains this validated
            // thunk; the SDK retains its exclusive stack record. Native
            // exclusion begins before it acquires a module data lock.
            let status = unsafe { callback(state) };
            if status > OK { BAD_ABI } else { status }
        })
    };
    operation().unwrap_or_else(core::convert::identity)
}
