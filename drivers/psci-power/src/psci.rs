//! Audited boundary for the machine's firmware power conduit.
//!
//! Hardware protocol: ARM PSCI, whose 64-bit calls carry the function in x0 and
//! return a status there. `SYSTEM_OFF` and `SYSTEM_RESET` are 0x84000008 and
//! 0x84000009; a successful call does not return, so a return is the failure.
//!
//! The conduit is not this driver's to choose. The bootstrap read `/psci` and its
//! `method` from the machine's own tree and the resident reports what it read;
//! this adapter implements `hvc` and refuses anything else by name, rather than
//! issuing an instruction the machine did not declare.
#![allow(unsafe_code)]
use mac_abi::boot::conduit;
use mac_driver::{Client, Error, Result, devices::DeviceService, power::Hardware};
use mac_power_interface::Action;

/// PSCI function identifiers, 64-bit calling convention.
const SYSTEM_OFF: u64 = 0x8400_0008;
const SYSTEM_RESET: u64 = 0x8400_0009;

pub struct Device;

impl Device
{
    pub fn initialize(host: Client) -> Result<Self>
    {
        let service = DeviceService::new(host)?;
        match service.power_conduit()?
        {
            conduit::HVC => Ok(Self),
            conduit::SMC =>
            {
                mac_driver::runtime::report(format_args!("POWER_CONDUIT_REFUSED: conduit=smc; \
                                                          this driver issues the hvc conduit \
                                                          only, so shutdown and restart are not \
                                                          offered rather than issued over an \
                                                          instruction the machine did not \
                                                          declare\n"));
                Err(Error(mac_abi::NOT_SUPPORTED))
            }
            conduit::OTHER =>
            {
                mac_driver::runtime::report(format_args!("POWER_CONDUIT_REFUSED: conduit=other; \
                                                          the machine's tree names a method the \
                                                          PSCI binding does not define, so no \
                                                          conduit is issued\n"));
                Err(Error(mac_abi::NOT_SUPPORTED))
            }
            _ =>
            {
                mac_driver::runtime::report(format_args!("POWER_CONDUIT_REFUSED: the machine's \
                                                          tree declares no firmware power \
                                                          conduit\n"));
                Err(Error(mac_abi::NOT_SUPPORTED))
            }
        }
    }
}

impl Hardware for Device
{
    fn trigger(&mut self, action: Action) -> Result<()>
    {
        let (name, function) = match action
        {
            Action::Shutdown => ("shutdown", SYSTEM_OFF),
            Action::Restart => ("restart", SYSTEM_RESET)
        };
        mac_driver::runtime::report(format_args!("POWER_REQUEST: action={name} \
                                                  psci=0x{function:x} conduit=hvc\n"));
        // SAFETY: PSCI defines both identifiers for exactly this call, and the
        // machine's tree declared the conduit that carries it. x0 carries the
        // function and receives the status; x0-x17 are caller-saved under the
        // PSCI calling convention, so nothing else is relied on across the call.
        let mut status = function;
        unsafe {
            core::arch::asm!("hvc #0",
                             inout("x0") status,
                             out("x1") _,
                             out("x2") _,
                             out("x3") _,
                             out("x4") _,
                             out("x5") _,
                             out("x6") _,
                             out("x7") _,
                             out("x8") _,
                             out("x9") _,
                             out("x10") _,
                             out("x11") _,
                             out("x12") _,
                             out("x13") _,
                             out("x14") _,
                             out("x15") _,
                             out("x16") _,
                             out("x17") _,
                             options(nostack))
        };
        // A successful SYSTEM_OFF or SYSTEM_RESET does not return to its caller.
        // Reaching here means the machine did not change state, which is reported
        // rather than mistaken for a shutdown that happened.
        mac_driver::runtime::report(format_args!("POWER_FAILED: action={name} \
                                                  psci=0x{function:x} returned \
                                                  status=0x{status:x}; the machine did not \
                                                  change state\n"));
        Err(Error(mac_abi::IO_ERROR))
    }
}
