//! System policy over capability-selected external power devices.
#![forbid(unsafe_code)]
use mac_abi::{BAD_ARGUMENT, NOT_SUPPORTED, Status, power::*};
use mac_power_interface::Action;

pub(crate) trait Host
{
    fn select(&mut self, capability: u64) -> Result<Option<u32>, Status>;
    fn applications_ready(&mut self) -> Result<(), Status>;
    fn flush(&mut self) -> Result<(), Status>;
    fn trigger(&mut self, provider: u32, action: Action) -> Result<(), Status>;
}

pub(crate) fn request(host: &mut impl Host, call: &mut PowerCall) -> Result<(), Status>
{
    if call.reserved != 0 || call.capabilities != 0 || call.failure_stage != 0
    {
        return Err(BAD_ARGUMENT);
    }
    call.failure_stage = SELECTION_FAILURE;
    if call.operation == CAPABILITIES
    {
        let mut flags = 0;
        if host.select(mac_power_interface::SHUTDOWN)?.is_some()
        {
            flags |= SHUTDOWN_AVAILABLE;
        }
        if host.select(mac_power_interface::RESTART)?.is_some()
        {
            flags |= RESTART_AVAILABLE;
        }
        call.capabilities = flags;
        call.failure_stage = 0;
        return Ok(());
    }
    let action = match call.operation
    {
        SHUTDOWN => Action::Shutdown,
        RESTART => Action::Restart,
        _ => return Err(BAD_ARGUMENT)
    };
    let provider = host.select(action.capability())?.ok_or(NOT_SUPPORTED)?;
    call.failure_stage = APPLICATION_FAILURE;
    host.applications_ready()?;
    // Refusal and version validation precede disk I/O. A failed flush must
    // never reach the hardware trigger, and no registry lock crosses callbacks.
    call.failure_stage = STORAGE_FAILURE;
    host.flush()?;
    call.failure_stage = DEVICE_FAILURE;
    host.trigger(provider, action)?;
    call.failure_stage = 0;
    Ok(())
}

#[cfg(target_arch = "riscv64")]
pub(crate) fn native(call: &mut PowerCall) -> Result<(), Status>
{
    struct Native;
    impl Host for Native
    {
        fn applications_ready(&mut self) -> Result<(), Status>
        {
            crate::kernel::with_desktop(|runtime|runtime.power_applications_ready())
                .map_err(|error|error.status())?
        }

        fn select(&mut self, capability: u64) -> Result<Option<u32>, Status>
        {
            match crate::drivers::find_kernel_provider(mac_power_interface::CLASS,
                                                       mac_power_interface::VERSION,
                                                       capability)
            {
                Ok(provider) => Ok(Some(provider)),
                Err(mac_abi::NOT_FOUND) => Ok(None),
                Err(error) => Err(error)
            }
        }

        fn flush(&mut self) -> Result<(), Status>
        {
            crate::kernel::with_files(|files| files.prepare_power()).map_err(|error| {
                                                                        error.status()
                                                                    })?
        }

        fn trigger(&mut self, provider: u32, action: Action) -> Result<(), Status>
        {
            let mut job = crate::drivers::submit_background_to(provider,
                                                               mac_power_interface::CLASS,
                                                               mac_power_interface::VERSION,
                                                               action.operation(),
                                                               alloc::vec::Vec::new(),
                                                               0)?;
            let completed = job.run_once()?.ok_or(mac_abi::SERVICE_BUSY)?;
            match completed.result
            {
                Ok(bytes) if bytes.is_empty() => Ok(()),
                Ok(_) => Err(mac_abi::BAD_ABI),
                Err(crate::drivers::jobs::Failure::Service(status)) => Err(status),
                Err(crate::drivers::jobs::Failure::Cancelled) => Err(mac_abi::OPERATION_CANCELLED)
            }
        }
    }
    request(&mut Native, call)
}
