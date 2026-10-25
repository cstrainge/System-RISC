//! System power policy. Applications never select a driver or access hardware.
#![no_std]
#![forbid(unsafe_code)]
use mac_abi::power::*;
use mac_traps::{Client, Result};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action
{
    Shutdown,
    Restart
}
impl core::fmt::Display for Action
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        f.write_str(match self
                    {
                        Self::Shutdown => "shut down",
                        Self::Restart => "restart"
                    })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage
{
    Selection,
    Applications,
    Storage,
    Device
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error
{
    pub action: Action,
    pub stage:  Stage,
    pub cause:  mac_traps::Error
}
impl core::fmt::Display for Error
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result
    {
        match (self.stage, self.cause)
        {
            (Stage::Applications, _) =>
            {
                write!(f,
                       "Cannot {} while other applications are running. Save your documents and \
                        quit those applications, then try again.",
                       self.action)
            }
            (Stage::Selection, mac_traps::Error::NotSupported | mac_traps::Error::NotFound) =>
            {
                write!(f,
                       "Cannot {}: no installed power device supports this action. Install a \
                        compatible driver in System Folder:Drivers.",
                       self.action)
            }
            (Stage::Selection, mac_traps::Error::BadAbi) =>
            {
                write!(f,
                       "Cannot {}: the power driver contract is incompatible. Rebuild or install \
                        a matching driver.",
                       self.action)
            }
            (_, mac_traps::Error::ServiceBusy) =>
            {
                write!(f,
                       "Cannot {} while file operations or device work are unfinished. Wait for \
                        them to finish, then try again.",
                       self.action)
            }
            (Stage::Storage, cause) =>
            {
                write!(f,
                       "Cannot {}: storage could not be made durable ({cause}). Resolve the \
                        storage error and try again; no power request was issued.",
                       self.action)
            }
            (_, cause) =>
            {
                write!(f,
                       "Cannot {}: the power device failed ({cause}). The hardware power request \
                        was not completed.",
                       self.action)
            }
        }
    }
}
impl core::error::Error for Error
{
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)>
    {
        Some(&self.cause)
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities
{
    pub shutdown: bool,
    pub restart:  bool
}
#[derive(Clone, Copy)]
pub struct Power<'a>
{
    client: Client<'a>
}
impl<'a> Power<'a>
{
    pub const fn new(client: Client<'a>) -> Self
    {
        Self { client }
    }

    pub fn capabilities(self) -> Result<Capabilities>
    {
        let mut call = PowerCall::default();
        self.client.power(&mut call)?;
        if call.failure_stage != 0
           || call.capabilities & !(SHUTDOWN_AVAILABLE | RESTART_AVAILABLE) != 0
        {
            return Err(mac_traps::Error::BadAbi);
        }
        Ok(Capabilities { shutdown: call.capabilities & SHUTDOWN_AVAILABLE != 0,
                          restart:  call.capabilities & RESTART_AVAILABLE != 0 })
    }

    pub fn shutdown(self) -> core::result::Result<(), Error>
    {
        self.request(Action::Shutdown)
    }

    pub fn restart(self) -> core::result::Result<(), Error>
    {
        self.request(Action::Restart)
    }

    fn request(self, action: Action) -> core::result::Result<(), Error>
    {
        let operation = match action
        {
            Action::Shutdown => SHUTDOWN,
            Action::Restart => RESTART
        };
        let mut call = PowerCall { operation,
                                   ..Default::default() };
        self.client.power(&mut call).map_err(|mut cause| {
                                        let stage = match call.failure_stage
                                        {
                                            SELECTION_FAILURE => Stage::Selection,
                                            APPLICATION_FAILURE => Stage::Applications,
                                            STORAGE_FAILURE => Stage::Storage,
                                            DEVICE_FAILURE => Stage::Device,
                                            _ =>
                                            {
                                                cause = mac_traps::Error::BadAbi;
                                                Stage::Selection
                                            }
                                        };
                                        Error { action,
                                                stage,
                                                cause }
                                    })
    }
}
