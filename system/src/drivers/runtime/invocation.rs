//! Genuine module executor scopes. The source/code lifetime is independent of
//! the original I/O subject and its allocation account.
#![forbid(unsafe_code)]
use mac_abi::Status;

pub(super) fn enter<R>(execution: &crate::images::ModuleExecution, call: impl FnOnce() -> R)
                       -> Result<R, Status>
{
    crate::context::with_module_execution(execution, || {
        crate::kernel::with_module_resources(execution, call)
    })?
}

pub(super) fn callback_allowed(module: u64, address: usize) -> Result<(), Status>
{
    crate::context::module_callback_allowed(module, address)
}
